//! 物理アドレス空間（Go の bus パッケージ）。RAM 領域と MMIO 領域を登録し、
//! アクセスをディスパッチする。SoC 固有のアドレスは持たない（machine が登録する）。
//!
//! Go との違い（所有権の設計、計画書 §3.3 の案 A）:
//!   - MMIO のデバイスはバスではなくボードが持つ。領域にはボードが決めた
//!     デバイスの識別子 `D` を登録し、読み書きは [`Devices`] を実装したボードに渡す。
//!   - RAM は 1 本の配列（アリーナ）にまとめ、ソフト TLB はスライスではなく
//!     アリーナ内の位置（[`RamOff`]）を持つ（Rust では TLB がバスへの参照を
//!     持ち続けられないため）。
//!   - 監視（-watch）はコールバックではなく、アクセスの記録を `watch_log` に
//!     ためる。呼び出し側（CLI）が取り出して表示する。

use std::fmt;

/// MMIO デバイスの読み書き（ボードが実装する）。`off` は領域先頭からの
/// オフセット、`size` は 1/2/4 バイト。周辺機器レジスタは副作用（FIFO 進行
/// など）があるため、サイズ情報ごと渡す。
pub trait Devices<D> {
    fn read(&mut self, dev: D, off: u32, size: u32) -> u32;
    fn write(&mut self, dev: D, off: u32, size: u32, v: u32);
    /// 32 ビットで読んだときに `read` が返す値を、副作用なしで返せる場合だけ返す
    /// （Go の bus.StableReader。CPU のアイドルスキップ用）。
    ///
    /// Some を返してよいのは、(1) その読み出しに副作用がなく、(2) 値が
    /// 「デバイスのイベント（時間経過による状態変化。machine が NextEvent で
    /// 管理する）・バスからの書き込み・外部入力」以外では変わらない場合だけ。
    /// ポーリングで待たれるステータスレジスタ（変換完了フラグ等）が対象。
    fn stable_read(&mut self, dev: D, off: u32, size: u32) -> Option<u32>;
}

/// 未マップアドレスへのアクセス。実機ならバスフォールト相当（エミュレーション停止）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BusError {
    pub addr: u32,
    pub write: bool,
}

impl fmt::Display for BusError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kind = if self.write { "write" } else { "read" };
        write!(f, "bus: {kind} to unmapped address {:08X}", self.addr)
    }
}

impl std::error::Error for BusError {}

/// 領域の登録のエラー（構成の誤り）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MapError(pub String);

impl fmt::Display for MapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "bus: {}", self.0)
    }
}

impl std::error::Error for MapError {}

/// RAM アリーナ内の位置（バイト単位）。
pub type RamOff = u32;

/// 監視範囲へのアクセスの記録（デバッグ用。-watch の表示）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WatchEvent {
    pub region: &'static str,
    pub addr: u32,
    pub size: u32,
    /// 読み出し値または書き込み値
    pub value: u32,
    pub write: bool,
}

#[derive(Clone, Copy, Debug)]
enum Kind<D> {
    /// アリーナの `off` から始まる RAM。`mask` が 0 以外なら部分デコード:
    /// 領域内オフセットを &= mask して RAM に届く。
    Ram {
        off: RamOff,
        len: u32,
        mask: u32,
    },
    Mmio(D),
}

#[derive(Clone, Debug)]
struct Region<D> {
    base: u32,
    size: u32,
    name: &'static str,
    kind: Kind<D>,
}

const PAGE_SHIFT: u32 = 12;
/// 引き表で「このページは領域が一部だけ覆う（線形探索に落とす）」を表す値。
const PAGE_PARTIAL: u16 = 0xFFFF;

/// 物理アドレス空間。
///
/// 領域検索は全アクセスで走るので、4KB ページ単位の引き表（pages）で
/// O(1) にしている（線形探索だと Go 版でプロファイルの 1 割近くを占めた）。
/// 4KB に揃っていない領域が一部だけ覆うページは PAGE_PARTIAL にして
/// 線形探索に落とす（現状の構成には無いが、正しさのための保険）。
pub struct Bus<D> {
    regions: Vec<Region<D>>,
    /// PA>>12 → 領域番号+1（0 = 未マップ、PAGE_PARTIAL = 線形探索）
    pages: Vec<u16>,
    /// すべての RAM 領域の実体
    arena: Vec<u8>,
    /// 監視範囲（両端を含む）。has_watch が false の間は RAM アクセスの経路に
    /// bool 判定 1 回ぶんしかコストを足さない。
    watches: Vec<(u32, u32)>,
    has_watch: bool,
    /// 監視範囲へのアクセスの記録（呼び出し側が取り出して空にする）。
    pub watch_log: Vec<WatchEvent>,
}

impl<D: Copy> Default for Bus<D> {
    fn default() -> Self {
        Self::new()
    }
}

impl<D: Copy> Bus<D> {
    pub fn new() -> Self {
        Bus {
            regions: vec![],
            pages: vec![0; 1 << (32 - PAGE_SHIFT)],
            arena: vec![],
            watches: vec![],
            has_watch: false,
            watch_log: vec![],
        }
    }

    /// base から size バイトの RAM を確保して配置する。
    pub fn map_ram(&mut self, name: &'static str, base: u32, size: u32) -> Result<(), MapError> {
        let off = self.alloc(name, size)?;
        self.add(Region {
            base,
            size,
            name,
            kind: Kind::Ram {
                off,
                len: size,
                mask: 0,
            },
        })
    }

    /// window バイトの窓に size バイトの RAM を折り返しで見せる。
    /// 実機のメモリコントローラはバンク窓（例: S3C2410 は 128MB）に対して
    /// 実装 RAM が小さいとき、アドレス線の部分デコードでエイリアスが生じる。
    /// OS のメモリサイズ検出はこの折り返しに依存するので再現する（今の構成では
    /// 使っていないが、Go と同じくテストごと持つ）。size は 2 の冪であること。
    pub fn map_ram_mirror(
        &mut self,
        name: &'static str,
        base: u32,
        window: u32,
        size: u32,
    ) -> Result<(), MapError> {
        if size == 0 || !size.is_power_of_two() || window < size {
            return Err(MapError(format!(
                "map_ram_mirror {name}: size {size:X} must be a power of two <= window {window:X}"
            )));
        }
        let off = self.alloc(name, size)?;
        self.add(Region {
            base,
            size: window,
            name,
            kind: Kind::Ram {
                off,
                len: size,
                mask: size - 1,
            },
        })
    }

    /// base から size バイトを MMIO としてデバイス dev に接続する。
    pub fn map_mmio(
        &mut self,
        name: &'static str,
        base: u32,
        size: u32,
        dev: D,
    ) -> Result<(), MapError> {
        self.add(Region {
            base,
            size,
            name,
            kind: Kind::Mmio(dev),
        })
    }

    fn alloc(&mut self, name: &str, size: u32) -> Result<RamOff, MapError> {
        let off = self.arena.len();
        match u32::try_from(off as u64 + size as u64) {
            Ok(_) => {
                self.arena.resize(off + size as usize, 0);
                Ok(off as RamOff)
            }
            Err(_) => Err(MapError(format!("{name}: RAM arena exceeds 4GB"))),
        }
    }

    fn add(&mut self, r: Region<D>) -> Result<(), MapError> {
        let end = r.base as u64 + r.size as u64;
        if r.size == 0 || end > 1 << 32 {
            return Err(MapError(format!(
                "region {} ({:08X}+{:X}) is empty or wraps",
                r.name, r.base, r.size
            )));
        }
        for x in &self.regions {
            if (r.base as u64) < x.base as u64 + x.size as u64 && (x.base as u64) < end {
                return Err(MapError(format!(
                    "region {} ({:08X}+{:X}) overlaps {} ({:08X}+{:X})",
                    r.name, r.base, r.size, x.name, x.base, x.size
                )));
            }
        }
        if self.regions.len() >= PAGE_PARTIAL as usize - 1 {
            return Err(MapError("too many regions".into()));
        }
        self.regions.push(r);
        let idx = self.regions.len() as u16; // 領域番号+1
        let base = self.regions[idx as usize - 1].base as u64;
        let mut p = base >> PAGE_SHIFT;
        while p << PAGE_SHIFT < end {
            let full = p << PAGE_SHIFT >= base && (p + 1) << PAGE_SHIFT <= end;
            let slot = &mut self.pages[p as usize];
            *slot = if full && *slot == 0 {
                idx
            } else {
                PAGE_PARTIAL
            };
            p += 1;
        }
        Ok(())
    }

    /// 物理アドレス lo〜hi（両端含む）へのアクセスを watch_log に記録させる。
    pub fn add_watch(&mut self, lo: u32, hi: u32) {
        self.watches.push((lo, hi));
        self.has_watch = true;
    }

    /// 監視が有効か。
    pub fn watching(&self) -> bool {
        self.has_watch
    }

    fn notify(&mut self, name: &'static str, addr: u32, size: u32, value: u32, write: bool) {
        if self
            .watches
            .iter()
            .any(|&(lo, hi)| addr >= lo && addr <= hi)
        {
            self.watch_log.push(WatchEvent {
                region: name,
                addr,
                size,
                value,
                write,
            });
        }
    }

    fn find(&self, addr: u32) -> Option<&Region<D>> {
        match self.pages[(addr >> PAGE_SHIFT) as usize] {
            0 => None,
            PAGE_PARTIAL => self
                .regions
                .iter()
                .find(|r| addr >= r.base && addr - r.base < r.size),
            i => Some(&self.regions[i as usize - 1]),
        }
    }

    /// PA を含む 4KB ページが RAM なら、そのページ先頭のアリーナ内の位置を返す。
    /// MMU のソフト TLB が RAM を直接読み書きする fast path 用。
    /// 監視が有効な間は None を返して必ずバスを経由させる（fast path だと
    /// 監視が素通りになるため）。
    pub fn ram_page(&self, pa: u32) -> Option<RamOff> {
        if self.has_watch {
            return None;
        }
        let r = self.find(pa)?;
        let Kind::Ram { off, len, mask } = r.kind else {
            return None;
        };
        let page = pa & !((1 << PAGE_SHIFT) - 1);
        if page < r.base || page as u64 + (1 << PAGE_SHIFT) > r.base as u64 + r.size as u64 {
            return None; // ページが領域をはみ出す
        }
        let mut o = page - r.base;
        if mask != 0 {
            o &= mask;
        }
        if o as u64 + (1 << PAGE_SHIFT) > len as u64 {
            return None;
        }
        Some(off + o)
    }

    /// RAM アリーナ全体（ソフト TLB の fast path・デコードキャッシュが
    /// [`ram_page`](Self::ram_page) の位置で読み書きする）。
    #[inline(always)]
    pub fn arena(&self) -> &[u8] {
        &self.arena
    }

    #[inline(always)]
    pub fn arena_mut(&mut self) -> &mut [u8] {
        &mut self.arena
    }

    /// addr を含む RAM 領域の実体と領域内オフセットを返す（ローダー・ハッシュ用）。
    pub fn ram(&self, addr: u32) -> Option<(&[u8], u32)> {
        let (off, len, o) = self.ram_loc(addr)?;
        Some((&self.arena[off as usize..(off + len) as usize], o))
    }

    pub fn ram_mut(&mut self, addr: u32) -> Option<(&mut [u8], u32)> {
        let (off, len, o) = self.ram_loc(addr)?;
        Some((&mut self.arena[off as usize..(off + len) as usize], o))
    }

    fn ram_loc(&self, addr: u32) -> Option<(RamOff, u32, u32)> {
        self.regions.iter().find_map(|r| match r.kind {
            Kind::Ram { off, len, mask } if addr >= r.base && addr - r.base < r.size => {
                let mut o = addr - r.base;
                if mask != 0 {
                    o &= mask;
                }
                Some((off, len, o))
            }
            _ => None,
        })
    }

    /// 物理アドレスの読み出し（size は 1/2/4）。
    pub fn read(
        &mut self,
        addr: u32,
        size: u32,
        devs: &mut impl Devices<D>,
    ) -> Result<u32, BusError> {
        let v = self.read1(addr, size, devs)?;
        if self.has_watch {
            let name = self.find(addr).map_or("", |r| r.name);
            self.notify(name, addr, size, v, false);
        }
        Ok(v)
    }

    fn read1(&mut self, addr: u32, size: u32, devs: &mut impl Devices<D>) -> Result<u32, BusError> {
        let err = BusError { addr, write: false };
        let r = self.find(addr).ok_or(err)?;
        if (addr - r.base) as u64 + size as u64 > r.size as u64 {
            return Err(err);
        }
        let mut o = addr - r.base;
        match r.kind {
            Kind::Ram { off, len, mask } => {
                if mask != 0 {
                    o &= mask;
                    if o + size > len {
                        // 折り返し境界をまたぐアクセス。実機ならバイト単位で折り返すが、
                        // アラインされたアクセスでは起きないので異常として報告する。
                        return Err(err);
                    }
                }
                let a = (off + o) as usize;
                let m = &self.arena;
                Ok(match size {
                    1 => m[a] as u32,
                    2 => u16::from_le_bytes([m[a], m[a + 1]]) as u32,
                    _ => u32::from_le_bytes([m[a], m[a + 1], m[a + 2], m[a + 3]]),
                })
            }
            Kind::Mmio(d) => Ok(devs.read(d, o, size)),
        }
    }

    /// 物理アドレスへの書き込み（size は 1/2/4。v の下位 size バイトを書く）。
    pub fn write(
        &mut self,
        addr: u32,
        size: u32,
        v: u32,
        devs: &mut impl Devices<D>,
    ) -> Result<(), BusError> {
        let err = BusError { addr, write: true };
        let r = self.find(addr).ok_or(err)?;
        if (addr - r.base) as u64 + size as u64 > r.size as u64 {
            return Err(err);
        }
        let (name, base, kind) = (r.name, r.base, r.kind);
        if self.has_watch {
            self.notify(name, addr, size, v, true);
        }
        let mut o = addr - base;
        match kind {
            Kind::Ram { off, len, mask } => {
                if mask != 0 {
                    o &= mask;
                    if o + size > len {
                        return Err(err); // read 側と同じ扱い
                    }
                }
                let a = (off + o) as usize;
                self.arena[a..a + size as usize].copy_from_slice(&v.to_le_bytes()[..size as usize]);
                Ok(())
            }
            Kind::Mmio(d) => {
                devs.write(d, o, size, v);
                Ok(())
            }
        }
    }

    /// 物理アドレスの 32 ビット読み出しを、副作用なしで行える場合だけ行う
    /// （MMU の probe32 から使う）。RAM はそのまま読み、MMIO は
    /// [`Devices::stable_read`] が応じるレジスタだけ。監視中は、実際のアクセス
    /// なら記録が残るので常に不可。
    pub fn probe32(&mut self, addr: u32, devs: &mut impl Devices<D>) -> Option<u32> {
        if self.has_watch || addr & 3 != 0 {
            return None;
        }
        let r = self.find(addr)?;
        if (addr - r.base) as u64 + 4 > r.size as u64 {
            return None;
        }
        match r.kind {
            Kind::Ram { .. } => self.read1(addr, 4, devs).ok(),
            Kind::Mmio(d) => devs.stable_read(d, addr - r.base, 4),
        }
    }

    /// MMIO 領域を登録順に返す（名前・先頭アドレス・デバイス）。
    pub fn mmio_regions(&self) -> impl Iterator<Item = (&'static str, u32, D)> + '_ {
        self.regions.iter().filter_map(|r| match r.kind {
            Kind::Mmio(d) => Some((r.name, r.base, d)),
            Kind::Ram { .. } => None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// テスト用のデバイス群。デバイス番号ごとに読み出し値と最後のアクセスを持つ。
    /// 番号 1 だけオフセット 0 を stable_read で読める。
    #[derive(Default)]
    struct Stub {
        read_val: [u32; 4],
        last: Option<(u8, u32, u32, u32)>, // (dev, off, size, val)
    }

    impl Devices<u8> for Stub {
        fn read(&mut self, dev: u8, off: u32, size: u32) -> u32 {
            self.last = Some((dev, off, size, 0));
            self.read_val[dev as usize]
        }
        fn write(&mut self, dev: u8, off: u32, size: u32, v: u32) {
            self.last = Some((dev, off, size, v));
        }
        fn stable_read(&mut self, dev: u8, off: u32, size: u32) -> Option<u32> {
            (dev == 1 && off == 0).then(|| self.read(dev, off, size))
        }
    }

    #[test]
    fn ram_read_write() {
        let (mut b, mut d) = (Bus::<u8>::new(), Stub::default());
        b.map_ram("ram", 0x1000, 0x100).unwrap();
        b.write(0x1010, 4, 0xAABBCCDD, &mut d).unwrap();
        // リトルエンディアン確認
        assert_eq!(b.read(0x1010, 1, &mut d), Ok(0xDD));
        assert_eq!(b.read(0x1012, 2, &mut d), Ok(0xAABB));
        assert_eq!(b.read(0x1010, 4, &mut d), Ok(0xAABBCCDD));
        // 8/16 ビットの書き込みは下位だけを書く
        b.write(0x1011, 1, 0x1234_5611, &mut d).unwrap();
        b.write(0x1012, 2, 0xFFFF_2233, &mut d).unwrap();
        assert_eq!(b.read(0x1010, 4, &mut d), Ok(0x223311DD));
    }

    #[test]
    fn unmapped_access() {
        let (mut b, mut d) = (Bus::<u8>::new(), Stub::default());
        b.map_ram("ram", 0x1000, 0x100).unwrap();
        assert_eq!(
            b.read(0x2000, 4, &mut d),
            Err(BusError {
                addr: 0x2000,
                write: false
            })
        );
        assert_eq!(
            b.write(0x0FFF, 1, 1, &mut d),
            Err(BusError {
                addr: 0x0FFF,
                write: true
            })
        );
        // 領域末尾をまたぐアクセスもエラー
        assert!(b.read(0x10FE, 4, &mut d).is_err());
    }

    #[test]
    fn mmio_dispatch() {
        let (mut b, mut d) = (Bus::<u8>::new(), Stub::default());
        d.read_val[2] = 0x1234;
        b.map_mmio("dev", 0x5000, 0x100, 2).unwrap();
        assert_eq!(b.read(0x5020, 2, &mut d), Ok(0x1234));
        assert_eq!(d.last, Some((2, 0x20, 2, 0)));
        b.write(0x5044, 4, 0xCAFE, &mut d).unwrap();
        assert_eq!(d.last, Some((2, 0x44, 4, 0xCAFE)));
    }

    #[test]
    fn overlap_rejected() {
        let mut b = Bus::<u8>::new();
        b.map_ram("a", 0x1000, 0x100).unwrap();
        assert!(b.map_ram("b", 0x10F0, 0x100).is_err());
        assert!(b.map_ram("c", 0x1100, 0x100).is_ok());
        assert!(b.map_mmio("wrap", 0xFFFF_F000, 0x2000, 0).is_err());
    }

    #[test]
    fn ram_lookup() {
        let mut b = Bus::<u8>::new();
        b.map_ram("ram", 0x1000, 0x100).unwrap();
        let (ram, off) = b.ram(0x1040).unwrap();
        assert_eq!((off, ram.len()), (0x40, 0x100));
        assert!(b.ram(0x2000).is_none());
    }

    #[test]
    fn ram_mirror() {
        let (mut b, mut d) = (Bus::<u8>::new(), Stub::default());
        b.map_ram_mirror("m", 0x1000, 0x400, 0x100).unwrap();
        b.write(0x1004, 4, 0xCAFEBABE, &mut d).unwrap();
        // 0x100 ごとに折り返して同じ値が見える
        for a in [0x1004, 0x1104, 0x1204, 0x1304] {
            assert_eq!(b.read(a, 4, &mut d), Ok(0xCAFEBABE), "{a:X}");
        }
        // エイリアス先への書き込みは元にも見える
        b.write(0x1204, 4, 0x11111111, &mut d).unwrap();
        assert_eq!(b.read(0x1004, 4, &mut d), Ok(0x11111111));
        // 窓の外は BusError
        assert!(b.read(0x1400, 4, &mut d).is_err());
        // size が 2 の冪でない場合は拒否
        assert!(
            Bus::<u8>::new()
                .map_ram_mirror("bad", 0, 0x400, 0x300)
                .is_err()
        );
    }

    #[test]
    fn watch() {
        let (mut b, mut d) = (Bus::<u8>::new(), Stub::default());
        b.map_ram("ram", 0x1000, 0x100).unwrap();
        d.read_val[0] = 0x55;
        b.map_mmio("dev", 0x2000, 0x100, 0).unwrap();
        b.add_watch(0x1010, 0x1013);
        b.add_watch(0x2000, 0x20FF);
        b.write(0x1010, 4, 0x12345678, &mut d).unwrap(); // 範囲内
        b.write(0x1020, 4, 1, &mut d).unwrap(); // 範囲外: 記録なし
        b.read(0x1011, 1, &mut d).unwrap(); // 範囲内
        b.read(0x2004, 4, &mut d).unwrap(); // MMIO 範囲内
        let ev = |region, addr, size, value, write| WatchEvent {
            region,
            addr,
            size,
            value,
            write,
        };
        assert_eq!(
            b.watch_log,
            vec![
                ev("ram", 0x1010, 4, 0x12345678, true),
                ev("ram", 0x1011, 1, 0x56, false),
                ev("dev", 0x2004, 4, 0x55, false),
            ]
        );
    }

    #[test]
    fn unaligned_region_and_ram_page() {
        let (mut b, mut d) = (Bus::<u8>::new(), Stub::default());
        b.map_ram("ram", 0x10000, 0x2000).unwrap();
        // 4KB に揃っていない小さな MMIO 2 個が同じページに同居する。
        d.read_val[1] = 1;
        d.read_val[2] = 2;
        b.map_mmio("d1", 0x20000, 0x10, 1).unwrap();
        b.map_mmio("d2", 0x20100, 0x10, 2).unwrap();
        assert_eq!(b.read(0x20000, 4, &mut d), Ok(1));
        assert_eq!(b.read(0x20104, 4, &mut d), Ok(2));
        assert!(
            b.read(0x20080, 4, &mut d).is_err(),
            "gap between d1 and d2 must be unmapped"
        );

        b.write(0x11004, 4, 0xCAFEBABE, &mut d).unwrap();
        let pg = b.ram_page(0x11FFF).unwrap() as usize;
        assert_eq!(b.arena()[pg + 4], 0xBE);
        assert!(
            b.ram_page(0x20000).is_none(),
            "ram_page on MMIO must be None"
        );
        b.add_watch(0, 0);
        assert!(
            b.ram_page(0x11000).is_none(),
            "ram_page must be None while watching"
        );
    }

    #[test]
    fn probe32() {
        let (mut b, mut d) = (Bus::<u8>::new(), Stub::default());
        b.map_ram("ram", 0x1000, 0x1000).unwrap();
        d.read_val[1] = 0x1234;
        d.read_val[3] = 1;
        b.map_mmio("stable", 0x2000, 0x100, 1).unwrap();
        b.map_mmio("plain", 0x3000, 0x100, 3).unwrap();
        b.write(0x1010, 4, 0xCAFEBABE, &mut d).unwrap();
        for (addr, want) in [
            (0x1010, Some(0xCAFEBABE)), // RAM
            (0x1012, None),             // 非アライン
            (0x2000, Some(0x1234)),     // stable_read できるレジスタ
            (0x2004, None),             // stable_read が断るレジスタ
            (0x3000, None),             // stable_read に応じないデバイス
            (0x9000, None),             // 未マップ
        ] {
            assert_eq!(b.probe32(addr, &mut d), want, "{addr:X}");
        }
        // 監視中は、実アクセスなら記録が残るので常に不可。
        b.add_watch(0x9000, 0x9000);
        assert_eq!(b.probe32(0x1010, &mut d), None);
    }
}
