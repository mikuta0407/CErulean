//! ストレージカードのディスクイメージ（MBR＋FAT12/16/32）を外から読み書きする。
//!
//! 目的: エミュレータの外（CLI・ブラウザ）から、カードに入れるファイルを
//! 出し入れする（2026-09-29 ユーザー決定: 「抜く → イメージを編集 → 挿す」）。
//! コアとは別のクレートで、コアからは使わない（ゲストから見える動作に関わらない）。
//! std のみ・プラットフォーム非依存（ファイル・時計に触れない。時刻は呼び出し側が渡す）。
//!
//! 一次資料: Microsoft Extensible Firmware Initiative FAT32 File System
//! Specification 1.03（fatgen103。以下「FS」）。MBR の区画表は FS の範囲外なので、
//! 区画の開始セクタ・セクタ数・種類の 3 つだけを読み書きする（CHS の欄は 0）。
//!
//! 作れるのは FAT16（MBR の区画 1 個）だけ。読み書きは FAT12/16/32 と、MBR のない
//! イメージ（先頭が BPB）の両方に対応する。
//!
//! 長いファイル名（LFN。FS 7 章）を読み書きする。書くときは、8.3 の大文字の名前で
//! 表せない名前に LFN を付け、短い名前は FS の「数字の尾」（~N）で作る。

use std::fmt;

pub const SECTOR: usize = 512;

/// 作れるイメージの大きさの範囲（FAT16 のクラスタ数の範囲に収まるもの。FS 3.5 の
/// DskTableFAT16: 8400 セクタ以下は FAT16 にできない。上限はコアのカードの上限）。
pub const MIN_FORMAT_BYTES: u64 = 8 << 20;
pub const MAX_FORMAT_BYTES: u64 = 512 << 20;

/// 区画の開始セクタ（作るとき。先頭の区画表のセクタから離しておく）。
const PART_START: u32 = 32;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error(pub String);

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

fn err<T>(m: impl Into<String>) -> Result<T, Error> {
    Err(Error(m.into()))
}

/// ディレクトリ項目の時刻（ローカル時刻。FAT は 2 秒単位・1980〜2107 年）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Timestamp {
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
}

impl Timestamp {
    /// FS 6.1 の日付と時刻。範囲外は 1980-01-01 00:00:00 にする。
    fn to_fat(self) -> (u16, u16) {
        let ok = (1980..=2107).contains(&self.year)
            && (1..=12).contains(&self.month)
            && (1..=31).contains(&self.day)
            && self.hour < 24
            && self.minute < 60
            && self.second < 60;
        if !ok {
            return (1 << 5 | 1, 0);
        }
        let date = (self.year - 1980) << 9 | (self.month as u16) << 5 | self.day as u16;
        let time = (self.hour as u16) << 11 | (self.minute as u16) << 5 | (self.second / 2) as u16;
        (date, time)
    }

    fn from_fat(date: u16, time: u16) -> Timestamp {
        Timestamp {
            year: 1980 + (date >> 9),
            month: ((date >> 5) & 0xF) as u8,
            day: (date & 0x1F) as u8,
            hour: (time >> 11) as u8,
            minute: ((time >> 5) & 0x3F) as u8,
            second: ((time & 0x1F) * 2) as u8,
        }
    }
}

/// ディレクトリの一覧の 1 項目。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    pub name: String,
    pub is_dir: bool,
    pub size: u32,
    pub modified: Timestamp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FatType {
    Fat12,
    Fat16,
    Fat32,
}

// ディレクトリ項目の属性（FS 6 章）
const ATTR_READ_ONLY: u8 = 0x01;
const ATTR_HIDDEN: u8 = 0x02;
const ATTR_SYSTEM: u8 = 0x04;
const ATTR_VOLUME_ID: u8 = 0x08;
const ATTR_DIRECTORY: u8 = 0x10;
const ATTR_ARCHIVE: u8 = 0x20;
const ATTR_LONG_NAME: u8 = ATTR_READ_ONLY | ATTR_HIDDEN | ATTR_SYSTEM | ATTR_VOLUME_ID;

fn le16(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
fn le32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}
fn put16(b: &mut [u8], o: usize, v: u16) {
    b[o..o + 2].copy_from_slice(&v.to_le_bytes());
}
fn put32(b: &mut [u8], o: usize, v: u32) {
    b[o..o + 4].copy_from_slice(&v.to_le_bytes());
}

/// FAT16 の区画 1 個の空のイメージを作る（FS 3 章の BPB と 3.5 の FATSz の式）。
/// size は 512 の倍数で MIN_FORMAT_BYTES〜MAX_FORMAT_BYTES。
pub fn format(size: u64, label: &str) -> Result<Vec<u8>, Error> {
    if !size.is_multiple_of(SECTOR as u64) || !(MIN_FORMAT_BYTES..=MAX_FORMAT_BYTES).contains(&size)
    {
        return err(format!(
            "card size must be a multiple of 512 between {} MB and {} MB",
            MIN_FORMAT_BYTES >> 20,
            MAX_FORMAT_BYTES >> 20
        ));
    }
    let total = (size / SECTOR as u64) as u32;
    let mut d = vec![0u8; size as usize];
    let vol_secs = total - PART_START;

    // MBR: 区画 1（FAT16。32MB 未満は 04h、以上は 06h）
    let ptype = if vol_secs < 65536 { 0x04 } else { 0x06 };
    let e = 446;
    d[e + 4] = ptype;
    put32(&mut d, e + 8, PART_START);
    put32(&mut d, e + 12, vol_secs);
    d[510] = 0x55;
    d[511] = 0xAA;

    // BPB（FS 3.1〜3.3）。クラスタの大きさは FS 3.5 の DskTableFAT16。
    let spc: u32 = match vol_secs {
        0..=32680 => 2,
        32681..=262144 => 4,
        262145..=524288 => 8,
        _ => 16,
    };
    let rsvd = 1u32;
    let nfats = 2u32;
    let root_ent = 512u32;
    let root_secs = (root_ent * 32).div_ceil(SECTOR as u32);
    let tmp1 = vol_secs - (rsvd + root_secs);
    let tmp2 = 256 * spc + nfats;
    let fatsz = tmp1.div_ceil(tmp2);
    let b = PART_START as usize * SECTOR;
    let bs = &mut d[b..b + SECTOR];
    bs[0..3].copy_from_slice(&[0xEB, 0x3C, 0x90]);
    bs[3..11].copy_from_slice(b"CERULEAN");
    put16(bs, 11, SECTOR as u16);
    bs[13] = spc as u8;
    put16(bs, 14, rsvd as u16);
    bs[16] = nfats as u8;
    put16(bs, 17, root_ent as u16);
    if vol_secs < 65536 {
        put16(bs, 19, vol_secs as u16);
    } else {
        put32(bs, 32, vol_secs);
    }
    bs[21] = 0xF8;
    put16(bs, 22, fatsz as u16);
    put16(bs, 24, 32); // BPB_SecPerTrk（区画の配置と同じ仮の値）
    put16(bs, 26, 2); // BPB_NumHeads
    put32(bs, 28, PART_START); // BPB_HiddSec
    bs[36] = 0x80; // BS_DrvNum
    bs[38] = 0x29; // BS_BootSig
    put32(bs, 39, 0x2006_0102); // BS_VolID（決定論的な固定値）
    let mut lab = [b' '; 11];
    for (i, c) in label
        .bytes()
        .filter(|c| c.is_ascii_graphic() || *c == b' ')
        .take(11)
        .enumerate()
    {
        lab[i] = c.to_ascii_uppercase();
    }
    bs[43..54].copy_from_slice(&lab);
    bs[54..62].copy_from_slice(b"FAT16   ");
    bs[510] = 0x55;
    bs[511] = 0xAA;

    // FAT[0] = メディア記述子、FAT[1] = EOC（FS 4 章）
    for f in 0..nfats {
        let o = b + (rsvd + f * fatsz) as usize * SECTOR;
        put16(&mut d, o, 0xFFF8);
        put16(&mut d, o + 2, 0xFFFF);
    }
    // ルートディレクトリにボリュームラベルの項目（FS 6 章の ATTR_VOLUME_ID）
    if lab != [b' '; 11] {
        let o = b + (rsvd + nfats * fatsz) as usize * SECTOR;
        d[o..o + 11].copy_from_slice(&lab);
        d[o + 11] = ATTR_VOLUME_ID;
    }
    Ok(d)
}

/// ディレクトリの場所。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DirLoc {
    /// FAT12/16 の固定の大きさのルート
    FixedRoot,
    /// クラスタの鎖（サブディレクトリ・FAT32 のルート）
    Chain(u32),
}

/// ディレクトリ中で見つけた項目（短い名前の項目と、その前の LFN の項目の位置）。
#[derive(Debug, Clone)]
struct Found {
    name: String,
    attr: u8,
    cluster: u32,
    size: u32,
    mtime: Timestamp,
    /// 短い名前の項目の位置（イメージ中のバイト位置）
    slot: usize,
    /// LFN の項目の位置
    lfn_slots: Vec<usize>,
}

/// 開いたボリューム（ディスクイメージを借りて読み書きする）。
pub struct Fs<'a> {
    d: &'a mut [u8],
    /// ボリュームの先頭（イメージ中のバイト位置）
    base: usize,
    bps: usize,
    spc: u32,
    rsvd: u32,
    nfats: u32,
    fatsz: u32,
    root_ent: u32,
    root_clus: u32,
    ty: FatType,
    /// データ領域の先頭セクタ（ボリュームの先頭から）
    data_start: u32,
    /// クラスタ数（2〜clusters+1 が有効）
    clusters: u32,
}

impl<'a> Fs<'a> {
    /// イメージを開く。先頭が MBR なら最初の FAT の区画、BPB ならそのまま（MBR なし）。
    pub fn open(d: &'a mut [u8]) -> Result<Fs<'a>, Error> {
        if d.len() < SECTOR || le16(d, 510) != 0xAA55 {
            return err("not a disk image (no boot signature)");
        }
        let base = if looks_like_bpb(&d[..SECTOR]) {
            0
        } else {
            let mut found = None;
            for i in 0..4 {
                let e = 446 + 16 * i;
                let t = d[e + 4];
                let start = le32(d, e + 8) as usize;
                if matches!(t, 0x01 | 0x04 | 0x06 | 0x0B | 0x0C | 0x0E)
                    && start > 0
                    && (start + 1) * SECTOR <= d.len()
                {
                    found = Some(start * SECTOR);
                    break;
                }
            }
            found.ok_or_else(|| Error("no FAT partition in the partition table".into()))?
        };
        let bs = &d[base..base + SECTOR];
        if !looks_like_bpb(bs) {
            return err("the partition has no valid FAT boot sector");
        }
        let bps = le16(bs, 11) as usize;
        if bps != SECTOR {
            return err(format!("unsupported sector size {bps}"));
        }
        let spc = bs[13] as u32;
        let rsvd = le16(bs, 14) as u32;
        let nfats = bs[16] as u32;
        let root_ent = le16(bs, 17) as u32;
        let tot = if le16(bs, 19) != 0 {
            le16(bs, 19) as u32
        } else {
            le32(bs, 32)
        };
        let fatsz = if le16(bs, 22) != 0 {
            le16(bs, 22) as u32
        } else {
            le32(bs, 36)
        };
        let root_secs = (root_ent * 32).div_ceil(bps as u32);
        // FS 3.5: FAT の種類はクラスタ数だけで決まる
        let data_start = rsvd + nfats * fatsz + root_secs;
        if spc == 0 || nfats == 0 || fatsz == 0 || data_start >= tot {
            return err("bad BPB");
        }
        let clusters = (tot - data_start) / spc;
        let ty = if clusters < 4085 {
            FatType::Fat12
        } else if clusters < 65525 {
            FatType::Fat16
        } else {
            FatType::Fat32
        };
        let root_clus = if ty == FatType::Fat32 {
            le32(bs, 44)
        } else {
            0
        };
        if base + tot as usize * bps > d.len() {
            return err("the volume is larger than the image");
        }
        Ok(Fs {
            d,
            base,
            bps,
            spc,
            rsvd,
            nfats,
            fatsz,
            root_ent,
            root_clus,
            ty,
            data_start,
            clusters,
        })
    }

    pub fn fat_type(&self) -> FatType {
        self.ty
    }

    fn cluster_bytes(&self) -> usize {
        self.spc as usize * self.bps
    }

    fn cluster_off(&self, c: u32) -> usize {
        self.base + (self.data_start + (c - 2) * self.spc) as usize * self.bps
    }

    // ---- FAT ----

    fn fat_get(&self, c: u32) -> u32 {
        let fo = self.base + self.rsvd as usize * self.bps;
        match self.ty {
            FatType::Fat12 => {
                let o = fo + (c + c / 2) as usize;
                let v = le16(self.d, o) as u32;
                if c & 1 != 0 { v >> 4 } else { v & 0xFFF }
            }
            FatType::Fat16 => le16(self.d, fo + 2 * c as usize) as u32,
            FatType::Fat32 => le32(self.d, fo + 4 * c as usize) & 0x0FFF_FFFF,
        }
    }

    fn fat_set(&mut self, c: u32, v: u32) {
        for f in 0..self.nfats {
            let fo = self.base + (self.rsvd + f * self.fatsz) as usize * self.bps;
            match self.ty {
                FatType::Fat12 => {
                    let o = fo + (c + c / 2) as usize;
                    let old = le16(self.d, o);
                    let n = if c & 1 != 0 {
                        (old & 0x000F) | ((v as u16 & 0xFFF) << 4)
                    } else {
                        (old & 0xF000) | (v as u16 & 0xFFF)
                    };
                    put16(self.d, o, n);
                }
                FatType::Fat16 => put16(self.d, fo + 2 * c as usize, v as u16),
                FatType::Fat32 => {
                    // 上位 4 ビットは保持する（FS 4 章）
                    let o = fo + 4 * c as usize;
                    let old = le32(self.d, o);
                    put32(self.d, o, old & 0xF000_0000 | v & 0x0FFF_FFFF);
                }
            }
        }
    }

    fn is_eoc(&self, v: u32) -> bool {
        match self.ty {
            FatType::Fat12 => v >= 0xFF8,
            FatType::Fat16 => v >= 0xFFF8,
            FatType::Fat32 => v >= 0x0FFF_FFF8,
        }
    }

    fn eoc(&self) -> u32 {
        match self.ty {
            FatType::Fat12 => 0xFFF,
            FatType::Fat16 => 0xFFFF,
            FatType::Fat32 => 0x0FFF_FFFF,
        }
    }

    /// クラスタの鎖（壊れた鎖・循環では打ち切る）。
    fn chain(&self, first: u32) -> Result<Vec<u32>, Error> {
        let mut v = Vec::new();
        let mut c = first;
        while c >= 2 && !self.is_eoc(c) {
            if c > self.clusters + 1 || v.len() > self.clusters as usize {
                return err("broken cluster chain");
            }
            v.push(c);
            c = self.fat_get(c);
        }
        Ok(v)
    }

    fn free_clusters(&self) -> u32 {
        (2..self.clusters + 2)
            .filter(|&c| self.fat_get(c) == 0)
            .count() as u32
    }

    /// 空きの容量（バイト）。
    pub fn free_bytes(&self) -> u64 {
        self.free_clusters() as u64 * self.cluster_bytes() as u64
    }

    /// n 個のクラスタを確保して鎖にする（中身は 0 にする）。
    fn alloc(&mut self, n: u32) -> Result<Vec<u32>, Error> {
        let mut got = Vec::with_capacity(n as usize);
        let mut c = 2;
        while got.len() < n as usize {
            if c >= self.clusters + 2 {
                for &g in &got {
                    self.fat_set(g, 0);
                }
                return err("the card is full");
            }
            if self.fat_get(c) == 0 {
                self.fat_set(c, self.eoc()); // 確保中の印（途中で失敗したら戻す）
                got.push(c);
            }
            c += 1;
        }
        for w in got.windows(2) {
            self.fat_set(w[0], w[1]);
        }
        let cb = self.cluster_bytes();
        for &g in &got {
            let o = self.cluster_off(g);
            self.d[o..o + cb].fill(0);
        }
        self.invalidate_fsinfo();
        Ok(got)
    }

    fn free_chain(&mut self, first: u32) -> Result<(), Error> {
        for c in self.chain(first)? {
            self.fat_set(c, 0);
        }
        self.invalidate_fsinfo();
        Ok(())
    }

    /// FAT32 の FSInfo の空きの数と次の空きを「不明」にする（FS 5 章。書き換えた後は
    /// 正しい値を保てないので、OS に数え直させる）。
    fn invalidate_fsinfo(&mut self) {
        if self.ty != FatType::Fat32 {
            return;
        }
        let fsi = le16(self.d, self.base + 48) as usize;
        let o = self.base + fsi * self.bps;
        if fsi != 0 && le32(self.d, o) == 0x4161_5252 {
            put32(self.d, o + 488, 0xFFFF_FFFF);
            put32(self.d, o + 492, 0xFFFF_FFFF);
        }
    }

    // ---- ディレクトリ ----

    fn root(&self) -> DirLoc {
        if self.ty == FatType::Fat32 {
            DirLoc::Chain(self.root_clus)
        } else {
            DirLoc::FixedRoot
        }
    }

    /// ディレクトリの 32 バイトの項目の位置の列。
    fn slots(&self, loc: DirLoc) -> Result<Vec<usize>, Error> {
        match loc {
            DirLoc::FixedRoot => {
                let o = self.base + (self.rsvd + self.nfats * self.fatsz) as usize * self.bps;
                Ok((0..self.root_ent as usize).map(|i| o + 32 * i).collect())
            }
            DirLoc::Chain(first) => {
                let per = self.cluster_bytes() / 32;
                let mut v = Vec::new();
                for c in self.chain(first)? {
                    let o = self.cluster_off(c);
                    v.extend((0..per).map(|i| o + 32 * i));
                }
                Ok(v)
            }
        }
    }

    /// ディレクトリの項目（「.」「..」とボリュームラベルを除く）。
    fn read_dir(&self, loc: DirLoc) -> Result<Vec<Found>, Error> {
        let mut out = Vec::new();
        let mut lfn: Vec<Lfn> = Vec::new();
        for s in self.slots(loc)? {
            let e = &self.d[s..s + 32];
            if e[0] == 0x00 {
                break; // 以降は空き（FS 6 章）
            }
            if e[0] == 0xE5 {
                lfn.clear();
                continue;
            }
            let attr = e[11];
            if attr & 0x3F == ATTR_LONG_NAME {
                let mut chars = [0u16; 13];
                for (i, &o) in LFN_OFFSETS.iter().enumerate() {
                    chars[i] = le16(e, o);
                }
                lfn.push(Lfn {
                    ord: e[0],
                    sum: e[13],
                    chars,
                    slot: s,
                });
                if e[0] & 0x40 != 0 && lfn.len() > 1 {
                    // 新しい鎖の始まり（直前の途切れた鎖は捨てる）
                    let last = lfn.pop().unwrap();
                    lfn.clear();
                    lfn.push(last);
                }
                continue;
            }
            let short: [u8; 11] = e[0..11].try_into().unwrap();
            if attr & ATTR_VOLUME_ID != 0 || short[0] == b'.' {
                lfn.clear();
                continue;
            }
            let name = long_name(&lfn, &short).unwrap_or_else(|| short_display(&short));
            let hi = if self.ty == FatType::Fat32 {
                le16(e, 20) as u32
            } else {
                0
            };
            out.push(Found {
                name,
                attr,
                cluster: hi << 16 | le16(e, 26) as u32,
                size: le32(e, 28),
                mtime: Timestamp::from_fat(le16(e, 24), le16(e, 22)),
                slot: s,
                lfn_slots: lfn.iter().map(|x| x.slot).collect(),
            });
            lfn.clear();
        }
        Ok(out)
    }

    /// パス（"/" 区切り。大文字小文字は区別しない）を辿って項目を探す。
    fn lookup(&self, path: &str) -> Result<Option<(DirLoc, Found)>, Error> {
        let parts = split_path(path)?;
        let Some((last, dirs)) = parts.split_last() else {
            return Ok(None); // ルート
        };
        let dir = self.dir_loc(dirs)?;
        let found = self
            .read_dir(dir)?
            .into_iter()
            .find(|f| names_equal(&f.name, last));
        Ok(found.map(|f| (dir, f)))
    }

    fn dir_loc(&self, parts: &[String]) -> Result<DirLoc, Error> {
        let mut loc = self.root();
        for p in parts {
            let f = self
                .read_dir(loc)?
                .into_iter()
                .find(|f| names_equal(&f.name, p))
                .ok_or_else(|| Error(format!("{p}: no such directory")))?;
            if f.attr & ATTR_DIRECTORY == 0 {
                return err(format!("{p}: not a directory"));
            }
            loc = DirLoc::Chain(f.cluster);
        }
        Ok(loc)
    }

    /// ディレクトリの一覧（名前の順。大文字小文字を区別しない順）。
    pub fn list(&self, path: &str) -> Result<Vec<DirEntry>, Error> {
        let parts = split_path(path)?;
        let loc = self.dir_loc(&parts)?;
        let mut v: Vec<DirEntry> = self
            .read_dir(loc)?
            .into_iter()
            .map(|f| DirEntry {
                is_dir: f.attr & ATTR_DIRECTORY != 0,
                size: if f.attr & ATTR_DIRECTORY != 0 {
                    0
                } else {
                    f.size
                },
                modified: f.mtime,
                name: f.name,
            })
            .collect();
        v.sort_by_key(|a| a.name.to_lowercase());
        Ok(v)
    }

    /// ファイルの中身。
    pub fn read_file(&self, path: &str) -> Result<Vec<u8>, Error> {
        let Some((_, f)) = self.lookup(path)? else {
            return err(format!("{path}: is the root directory"));
        };
        if f.attr & ATTR_DIRECTORY != 0 {
            return err(format!("{path}: is a directory"));
        }
        let mut out = Vec::with_capacity(f.size as usize);
        let cb = self.cluster_bytes();
        for c in self.chain(f.cluster)? {
            let o = self.cluster_off(c);
            let n = (f.size as usize - out.len()).min(cb);
            out.extend_from_slice(&self.d[o..o + n]);
            if out.len() == f.size as usize {
                break;
            }
        }
        if out.len() != f.size as usize {
            return err(format!("{path}: cluster chain is shorter than the file"));
        }
        Ok(out)
    }

    /// ファイルを書く（あれば置き換える）。親のディレクトリは無ければ作る。
    pub fn write_file(&mut self, path: &str, data: &[u8], t: Timestamp) -> Result<(), Error> {
        let parts = split_path(path)?;
        let Some((name, dirs)) = parts.split_last() else {
            return err("cannot write to the root directory");
        };
        if data.len() > u32::MAX as usize {
            return err(format!("{path}: file too large for FAT"));
        }
        let dir = self.mkdir_all(dirs, t)?;
        if let Some(f) = self
            .read_dir(dir)?
            .into_iter()
            .find(|f| names_equal(&f.name, name))
        {
            if f.attr & ATTR_DIRECTORY != 0 {
                return err(format!("{path}: is a directory"));
            }
            self.remove_found(&f)?;
        }
        let cb = self.cluster_bytes();
        let n = data.len().div_ceil(cb) as u32;
        let chain = if n == 0 { vec![] } else { self.alloc(n)? };
        for (i, &c) in chain.iter().enumerate() {
            let o = self.cluster_off(c);
            let src = &data[i * cb..((i + 1) * cb).min(data.len())];
            self.d[o..o + src.len()].copy_from_slice(src);
        }
        let first = chain.first().copied().unwrap_or(0);
        if let Err(e) = self.add_entry(dir, name, ATTR_ARCHIVE, first, data.len() as u32, t) {
            if first != 0 {
                self.free_chain(first)?;
            }
            return Err(e);
        }
        Ok(())
    }

    /// ディレクトリを作る（親も無ければ作る。既にあれば何もしない）。
    pub fn mkdir(&mut self, path: &str, t: Timestamp) -> Result<(), Error> {
        let parts = split_path(path)?;
        self.mkdir_all(&parts, t).map(|_| ())
    }

    fn mkdir_all(&mut self, parts: &[String], t: Timestamp) -> Result<DirLoc, Error> {
        let mut loc = self.root();
        for p in parts {
            let found = self
                .read_dir(loc)?
                .into_iter()
                .find(|f| names_equal(&f.name, p));
            loc = match found {
                Some(f) if f.attr & ATTR_DIRECTORY != 0 => DirLoc::Chain(f.cluster),
                Some(_) => return err(format!("{p}: a file with this name exists")),
                None => {
                    let c = self.alloc(1)?[0];
                    // 「.」と「..」（FS 6.x: ルートを指す「..」のクラスタは 0）
                    let parent = match loc {
                        DirLoc::Chain(pc) if pc != self.root_clus || self.ty != FatType::Fat32 => {
                            pc
                        }
                        _ => 0,
                    };
                    let (date, time) = t.to_fat();
                    let o = self.cluster_off(c);
                    for (i, (nm, cl)) in [(*b".          ", c), (*b"..         ", parent)]
                        .into_iter()
                        .enumerate()
                    {
                        let e = &mut self.d[o + 32 * i..o + 32 * (i + 1)];
                        write_short_entry(e, &nm, ATTR_DIRECTORY, cl, 0, date, time, self.ty);
                    }
                    if let Err(e) = self.add_entry(loc, p, ATTR_DIRECTORY, c, 0, t) {
                        self.free_chain(c)?;
                        return Err(e);
                    }
                    DirLoc::Chain(c)
                }
            };
        }
        Ok(loc)
    }

    /// ファイルか空のディレクトリを消す。recursive なら中身ごと消す。
    pub fn remove(&mut self, path: &str, recursive: bool) -> Result<(), Error> {
        let Some((_, f)) = self.lookup(path)? else {
            return err("cannot remove the root directory");
        };
        if f.attr & ATTR_DIRECTORY != 0 {
            let children = self.read_dir(DirLoc::Chain(f.cluster))?;
            if !children.is_empty() {
                if !recursive {
                    return err(format!("{path}: directory is not empty"));
                }
                for ch in children {
                    let p = format!("{}/{}", path.trim_end_matches('/'), ch.name);
                    self.remove(&p, true)?;
                }
            }
        }
        self.remove_found(&f)
    }

    fn remove_found(&mut self, f: &Found) -> Result<(), Error> {
        if f.cluster != 0 {
            self.free_chain(f.cluster)?;
        }
        for &s in f.lfn_slots.iter().chain(std::iter::once(&f.slot)) {
            self.d[s] = 0xE5; // 削除済み（FS 6 章）
        }
        Ok(())
    }

    /// ディレクトリに項目（必要なら LFN 付き）を足す。空きが無ければ鎖を伸ばす
    /// （固定の大きさのルートは伸ばせない）。
    fn add_entry(
        &mut self,
        dir: DirLoc,
        name: &str,
        attr: u8,
        cluster: u32,
        size: u32,
        t: Timestamp,
    ) -> Result<(), Error> {
        let existing = self.read_dir(dir)?;
        let taken: Vec<[u8; 11]> = self
            .slots(dir)?
            .into_iter()
            .map(|s| self.d[s..s + 11].try_into().unwrap())
            .collect();
        let (short, needs_lfn) = short_name_for(name, &taken)?;
        if existing.iter().any(|f| names_equal(&f.name, name)) {
            return err(format!("{name}: already exists"));
        }
        let units: Vec<u16> = name.encode_utf16().collect();
        let n_lfn = if needs_lfn {
            units.len().div_ceil(13)
        } else {
            0
        };
        let need = n_lfn + 1;
        let slots = self.find_free_run(dir, need)?;
        let sum = lfn_checksum(&short);
        for (k, &s) in slots[..n_lfn].iter().enumerate() {
            // 並びは最後の断片が先頭（FS 7 章）
            let ord = (n_lfn - k) as u8;
            let e = &mut self.d[s..s + 32];
            e.fill(0);
            e[0] = ord | if k == 0 { 0x40 } else { 0 };
            e[11] = ATTR_LONG_NAME;
            e[13] = sum;
            for (i, &o) in LFN_OFFSETS.iter().enumerate() {
                let idx = (ord as usize - 1) * 13 + i;
                let v = match idx.cmp(&units.len()) {
                    std::cmp::Ordering::Less => units[idx],
                    std::cmp::Ordering::Equal => 0x0000,
                    std::cmp::Ordering::Greater => 0xFFFF,
                };
                put16(e, o, v);
            }
        }
        let (date, time) = t.to_fat();
        let s = slots[n_lfn];
        let ty = self.ty;
        write_short_entry(
            &mut self.d[s..s + 32],
            &short,
            attr,
            cluster,
            size,
            date,
            time,
            ty,
        );
        Ok(())
    }

    /// 連続した n 個の空き項目を探す（無ければ鎖を 1 クラスタ伸ばす）。
    fn find_free_run(&mut self, dir: DirLoc, n: usize) -> Result<Vec<usize>, Error> {
        loop {
            let slots = self.slots(dir)?;
            let mut run = Vec::new();
            let mut end_reached = false;
            for &s in &slots {
                let b = self.d[s];
                if end_reached || b == 0x00 || b == 0xE5 {
                    if b == 0x00 {
                        end_reached = true;
                    }
                    run.push(s);
                    if run.len() == n {
                        return Ok(run);
                    }
                } else {
                    run.clear();
                }
            }
            let DirLoc::Chain(first) = dir else {
                return err("the root directory is full");
            };
            let last = *self
                .chain(first)?
                .last()
                .ok_or_else(|| Error("empty directory chain".into()))?;
            let c = self.alloc(1)?[0];
            self.fat_set(last, c);
        }
    }
}

/// LFN の項目の中の 13 文字の位置（FS 7 章）。
const LFN_OFFSETS: [usize; 13] = [1, 3, 5, 7, 9, 14, 16, 18, 20, 22, 24, 28, 30];

fn looks_like_bpb(s: &[u8]) -> bool {
    (s[0] == 0xEB || s[0] == 0xE9)
        && le16(s, 11) == 512
        && s[13].is_power_of_two()
        && le16(s, 14) != 0
        && s[16] != 0
}

#[allow(clippy::too_many_arguments)]
fn write_short_entry(
    e: &mut [u8],
    name: &[u8; 11],
    attr: u8,
    cluster: u32,
    size: u32,
    date: u16,
    time: u16,
    ty: FatType,
) {
    e.fill(0);
    e[0..11].copy_from_slice(name);
    e[11] = attr;
    put16(e, 14, time); // 作成時刻
    put16(e, 16, date); // 作成日
    put16(e, 18, date); // 最終アクセス日
    if ty == FatType::Fat32 {
        put16(e, 20, (cluster >> 16) as u16);
    }
    put16(e, 22, time);
    put16(e, 24, date);
    put16(e, 26, cluster as u16);
    put32(e, 28, size);
}

/// LFN の短い名前のチェックサム（FS 7 章の ChkSum）。
fn lfn_checksum(short: &[u8; 11]) -> u8 {
    short.iter().fold(0u8, |s, &c| {
        (if s & 1 != 0 { 0x80u8 } else { 0 })
            .wrapping_add(s >> 1)
            .wrapping_add(c)
    })
}

/// LFN の 1 項目。
struct Lfn {
    ord: u8,
    sum: u8,
    chars: [u16; 13],
    slot: usize,
}

/// LFN の断片の列（ディスク上の順。最後の断片が先頭）から名前を組み立てる。
/// 順番（先頭が n|0x40、以降 n-1…1）とチェックサムが合わなければ None（FS 7 章）。
fn long_name(lfn: &[Lfn], short: &[u8; 11]) -> Option<String> {
    let n = lfn.len();
    if n == 0 {
        return None;
    }
    let sum = lfn_checksum(short);
    for (k, l) in lfn.iter().enumerate() {
        let want = (n - k) as u8 | if k == 0 { 0x40 } else { 0 };
        if l.ord != want || l.sum != sum {
            return None;
        }
    }
    let units: Vec<u16> = lfn.iter().rev().flat_map(|l| l.chars).collect();
    let end = units.iter().position(|&u| u == 0).unwrap_or(units.len());
    String::from_utf16(&units[..end]).ok()
}

/// 短い名前（空白で埋めた 11 バイト）を「NAME.EXT」にする。
fn short_display(s: &[u8; 11]) -> String {
    let mut b = s[0..8].to_vec();
    if b[0] == 0x05 {
        b[0] = 0xE5; // FS 6 章: 先頭の 0xE5 は 0x05 で表す
    }
    let base: String = b
        .iter()
        .map(|&c| c as char)
        .collect::<String>()
        .trim_end()
        .to_string();
    let ext: String = s[8..11]
        .iter()
        .map(|&c| c as char)
        .collect::<String>()
        .trim_end()
        .to_string();
    if ext.is_empty() {
        base
    } else {
        format!("{base}.{ext}")
    }
}

fn names_equal(a: &str, b: &str) -> bool {
    a.to_lowercase() == b.to_lowercase()
}

/// LFN に使えない文字（FS 7 章）。
fn invalid_long_char(c: char) -> bool {
    (c as u32) < 0x20 || "\"*/:<>?\\|".contains(c)
}

fn split_path(path: &str) -> Result<Vec<String>, Error> {
    let mut v = Vec::new();
    for p in path.split(['/', '\\']).filter(|p| !p.is_empty()) {
        if p == "." || p == ".." {
            return err(format!("{path}: '.' and '..' are not allowed"));
        }
        if p.chars().any(invalid_long_char) || p.ends_with(' ') || p.ends_with('.') {
            return err(format!("{p}: invalid file name"));
        }
        if p.encode_utf16().count() > 255 {
            return err(format!("{p}: name too long"));
        }
        v.push(p.to_string());
    }
    Ok(v)
}

/// 8.3 の短い名前に使える文字（FS 6.1。ASCII の範囲だけを使う）。
fn short_char_ok(c: u8) -> bool {
    c.is_ascii_uppercase() || c.is_ascii_digit() || b"$%'-_@~`!(){}^#&".contains(&c)
}

/// 名前に対する短い名前と、LFN が要るか。名前がそのまま大文字の 8.3 なら LFN なし。
/// そうでなければ FS 7 章の「数字の尾」の手順（基底の名前＋~N）で、taken と
/// 重ならない名前を作る。
fn short_name_for(name: &str, taken: &[[u8; 11]]) -> Result<([u8; 11], bool), Error> {
    let (base, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i + 1..]),
        _ => (name, ""),
    };
    let exact = !base.is_empty()
        && base.len() <= 8
        && ext.len() <= 3
        && base.bytes().chain(ext.bytes()).all(short_char_ok);
    let mut s = [b' '; 11];
    if exact {
        s[..base.len()].copy_from_slice(base.as_bytes());
        s[8..8 + ext.len()].copy_from_slice(ext.as_bytes());
        if s[0] == 0xE5 {
            s[0] = 0x05;
        }
        return Ok((s, false));
    }
    // 基底の名前: 大文字にして使えない文字を「_」に、空白と「.」を除く
    let conv = |t: &str| -> Vec<u8> {
        t.chars()
            .filter(|&c| c != ' ' && c != '.')
            .map(|c| {
                let u = c.to_ascii_uppercase();
                if u.is_ascii() && short_char_ok(u as u8) {
                    u as u8
                } else {
                    b'_'
                }
            })
            .collect()
    };
    let b = conv(base);
    let e = conv(ext);
    for (i, &c) in e.iter().take(3).enumerate() {
        s[8 + i] = c;
    }
    for n in 1..=999_999u32 {
        let tail = format!("~{n}");
        let keep = (8 - tail.len()).min(b.len());
        let mut cand = s;
        cand[..8].fill(b' ');
        cand[..keep].copy_from_slice(&b[..keep]);
        cand[keep..keep + tail.len()].copy_from_slice(tail.as_bytes());
        if !taken.contains(&cand) {
            return Ok((cand, true));
        }
    }
    err(format!("{name}: no free short name"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: Timestamp = Timestamp {
        year: 2006,
        month: 1,
        day: 2,
        hour: 15,
        minute: 4,
        second: 6,
    };

    #[test]
    fn format_and_round_trip() {
        let mut img = format(16 << 20, "CERULEAN").unwrap();
        let mut fs = Fs::open(&mut img).unwrap();
        assert_eq!(fs.fat_type(), FatType::Fat16);
        let free0 = fs.free_bytes();
        let big: Vec<u8> = (0..100_000u32).map(|i| i as u8).collect();
        fs.write_file("HELLO.TXT", b"hello", T).unwrap();
        fs.write_file("日本語の長い名前のファイル.txt", &big, T)
            .unwrap();
        fs.write_file("dir one/sub/Deep File.bin", b"x", T).unwrap();
        fs.write_file("empty", b"", T).unwrap();
        let root = fs.list("/").unwrap();
        let names: Vec<_> = root.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "dir one",
                "empty",
                "HELLO.TXT",
                "日本語の長い名前のファイル.txt"
            ]
        );
        assert_eq!(root[3].size, 100_000);
        assert_eq!(root[2].modified, T);
        assert_eq!(fs.read_file("hello.txt").unwrap(), b"hello");
        assert_eq!(fs.read_file("日本語の長い名前のファイル.TXT").unwrap(), big);
        assert_eq!(fs.read_file("/DIR ONE/sub/deep file.bin").unwrap(), b"x");
        // 置き換えと削除で空きが戻る
        fs.write_file("HELLO.TXT", b"bye", T).unwrap();
        assert_eq!(fs.read_file("HELLO.TXT").unwrap(), b"bye");
        assert!(fs.remove("dir one", false).is_err());
        fs.remove("dir one", true).unwrap();
        fs.remove("日本語の長い名前のファイル.txt", false).unwrap();
        fs.remove("HELLO.TXT", false).unwrap();
        fs.remove("empty", false).unwrap();
        assert_eq!(fs.free_bytes(), free0);
        assert!(fs.list("/").unwrap().is_empty());
    }

    #[test]
    fn short_names_get_numeric_tails() {
        let mut img = format(8 << 20, "").unwrap();
        let mut fs = Fs::open(&mut img).unwrap();
        for i in 0..12 {
            fs.write_file(&format!("Long File Name {i}.jpeg"), &[i as u8], T)
                .unwrap();
        }
        let l = fs.list("").unwrap();
        assert_eq!(l.len(), 12);
        for i in 0..12 {
            let n = format!("Long File Name {i}.jpeg");
            assert_eq!(fs.read_file(&n).unwrap(), [i as u8]);
        }
        // 短い名前は重ならない
        let root = fs.slots(fs.root()).unwrap();
        let mut shorts: Vec<[u8; 11]> = root
            .iter()
            .filter(|&&s| fs.d[s] != 0 && fs.d[s] != 0xE5 && fs.d[s + 11] & 0x3F != ATTR_LONG_NAME)
            .map(|&s| fs.d[s..s + 11].try_into().unwrap())
            .collect();
        let n = shorts.len();
        shorts.sort();
        shorts.dedup();
        assert_eq!(shorts.len(), n);
    }

    #[test]
    fn many_files_grow_a_subdirectory() {
        let mut img = format(8 << 20, "").unwrap();
        let mut fs = Fs::open(&mut img).unwrap();
        for i in 0..300 {
            fs.write_file(&format!("d/file number {i:03}.txt"), b"z", T)
                .unwrap();
        }
        assert_eq!(fs.list("d").unwrap().len(), 300);
    }

    #[test]
    fn rejects_bad_input() {
        assert!(format(1 << 20, "").is_err());
        let mut junk = vec![0u8; 4096];
        assert!(Fs::open(&mut junk).is_err());
        let mut img = format(8 << 20, "").unwrap();
        let mut fs = Fs::open(&mut img).unwrap();
        assert!(fs.write_file("a/../b", b"", T).is_err());
        assert!(fs.write_file("what?", b"", T).is_err());
        assert!(fs.read_file("missing").is_err());
    }
}
