//! ARM920T の MMU（CP15 System Control Coprocessor）。Go の mmu パッケージ。
//! 仕様の根拠は ARM Architecture Reference Manual (DDI 0100) 第B3章。
//!
//! 設計（ユーザー確認済み 2026-09）:
//!   - 変換結果は 4KB 単位のソフト TLB にキャッシュする（下の「ソフト TLB」）。
//!     CP15 の c1/c2/c3/c8 書き込みで全無効化する。
//!   - キャッシュ（c7、C/W/I ビット）はエミュレートしない（書き込み保持のみ）。
//!   - MMU 起因のフォルトは [`MemError::Abort`] で返し、CPU が ARM 例外に変換する。
//!     ページテーブル自体が未マップ物理を指した場合（[`MemError::Bus`]）は
//!     エミュレータ側の不備なので、そのまま返して停止させる。
//!   - 物理アクセスは [`PhysMem`]（ボードが bus とデバイスを束ねて渡す）経由。
//!
//! ソフト TLB（性能対策。ユーザー確認済み 2026-09）:
//!
//! 変換結果を 4KB ページ単位で直接マップ方式のキャッシュに持つ。毎アクセスの
//! テーブルウォーク（L1/L2 の物理読み出し 1〜2 回）を省き、変換先が RAM なら
//! RAM の実体を直接読み書きして bus も経由しない。
//!
//! 正しさの根拠: 実機（ARM920T）にも TLB があり、ページテーブルを書き換えた
//! OS は CP15 c8 で TLB を無効化する義務がある（ARM ARM B3.7）。したがって
//! 「c8 操作まで古い変換が残る」のは実機と同じ振る舞い。エミュレータの TLB は
//! 実機より大きい（エントリ数・置換規則が違う）が、無効化を怠るソフトは実機でも
//! 動作が不定なので問題にしない。フォルトになる変換はキャッシュしない
//! （実機の TLB もフォルト記述子は保持しない）ので、新規マッピングの追加は
//! 無効化なしでも即座に見える。
//!
//! **Go 版と一致させるため、埋める・捨てるタイミングまで同じにする**（ゲストが
//! 無効化を怠った場合に古い変換が使われるかがこの規則で決まる。計画書 §4.1）。
//!
//! 全無効化の契機: c1（M/S/R ビットが権限判定に効く）・c2（TTB）・c3（DACR）・
//! c8（TLB 操作。単一エントリ無効化も安全側で全無効化にする）の書き込み。
//! c13（FCSE PID）はタグを MVA にしているので無効化不要。
//!
//! キャッシュしない変換（毎回ウォークする）:
//!   - tiny ページ（1KB）と、サブページ（1KB）ごとに AP が異なる小ページ
//!     （4KB 内で権限が一様でないため）
//!   - MMU 有効化直後のフェッチ猶予中の命令フェッチ（[`Mmu::fetch32`] 参照）
//!
//! 命令フェッチの高速化の支援（下の「デコードキャッシュの支援」。Go の code.go）。

use crate::bus::{PhysMem, RamOff};
use crate::cpu::{Abort, MemError};

// 制御レジスタ（CP15 c1）の関心ビット。他のビット（C/W/P/D/L/B/I など）は
// 保持するだけで動作に影響しない（キャッシュ非エミュレートのため）。
pub const CTRL_M: u32 = 1 << 0; // MMU 有効
pub const CTRL_A: u32 = 1 << 1; // アライメントチェック（TODO: 未実装。translate 参照）
pub const CTRL_S: u32 = 1 << 8; // System 保護
pub const CTRL_R: u32 = 1 << 9; // ROM 保護
pub const CTRL_V: u32 = 1 << 13; // 例外ベクタを 0xFFFF0000 に

// フォルトステータス符号（ARM ARM B3-19 Table）。FSR[3:0] に入る。
pub const FS_ALIGN: u8 = 0x1;
pub const FS_TRANS_SECT: u8 = 0x5; // セクション変換フォルト
pub const FS_TRANS_PAGE: u8 = 0x7;
pub const FS_DOMAIN_SECT: u8 = 0x9;
pub const FS_DOMAIN_PAGE: u8 = 0xB;
pub const FS_PERM_SECT: u8 = 0xD;
pub const FS_PERM_PAGE: u8 = 0xF;

// 一次/二次記述子の下位 2 ビット（タイプ）。
const DESC_FAULT: u32 = 0;
const DESC_COARSE: u32 = 1; // 一次: 粗ページテーブル
const DESC_SECTION: u32 = 2; // 一次: セクション
const DESC_FINE: u32 = 3; // 一次: 細ページテーブル
const DESC_LARGE: u32 = 1; // 二次: 大ページ（64KB）
const DESC_SMALL: u32 = 2; // 二次: 小ページ（4KB）
// 二次の 3 は細ページ（1KB、fine テーブルのみ）

/// ARM920T の Main ID レジスタ値（ARM920T TRM）。
/// 0x41 = ARM Ltd, 920 = part number, rev は適当に 0。
const ARM920_MAIN_ID: u32 = 0x41129200;

/// ARM920T のキャッシュタイプレジスタ（c0, opc2=1）。
/// TODO: 0x0D172172 は ARM920T TRM の値のはずだが要再確認（記憶ベース）。
const ARM920_CACHE_TYPE: u32 = 0x0D172172;

pub const TLB_BITS: u32 = 10;
pub const TLB_SIZE: usize = 1 << TLB_BITS;
/// tag の有効ビット（MVA>>12 は 20 ビットなので衝突しない）。
const TLB_VALID: u32 = 1 << 31;

// 権限ビット（TlbEntry::perm）。特権/ユーザー × 読み/書き の 4 通りを
// フィル時にまとめて計算しておき、モード切替で TLB を捨てずに済ませる。
const PERM_PRIV_R: u8 = 1 << 0;
const PERM_PRIV_W: u8 = 1 << 1;
const PERM_USER_R: u8 = 1 << 2;
const PERM_USER_W: u8 = 1 << 3;
const PERM_ALL: u8 = PERM_PRIV_R | PERM_PRIV_W | PERM_USER_R | PERM_USER_W;

/// RAM の実体がないことを表す位置（MMIO のページなど）。
pub const NO_RAM: RamOff = RamOff::MAX;

/// ソフト TLB の 1 エントリ。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TlbEntry {
    /// MVA>>12 | TLB_VALID（保存する）
    pub tag: u32,
    /// 物理ページ先頭（保存する）
    pub pa: u32,
    /// 権限ビット（保存する）
    pub perm: u8,
    /// 物理ページが RAM ならその実体のアリーナ内の位置。MMIO なら NO_RAM
    /// （派生情報: pa から引き直せる）。
    pub ram: RamOff,
    /// 書き込みの fast path 用の実体の位置。ram と同じだが、コードページでは
    /// NO_RAM にして書き込みを遅い経路に回す（派生情報。デコードキャッシュ用）。
    pub wram: RamOff,
    /// CPU がこのエントリの変換を覚えている（code_page で渡した）印。
    /// 詰め替えるときに世代を上げる（派生情報）。
    pub watched: bool,
}

const EMPTY_ENTRY: TlbEntry = TlbEntry {
    tag: 0,
    pa: 0,
    perm: 0,
    ram: NO_RAM,
    wram: NO_RAM,
    watched: false,
};

/// CPU とバスの間に入る仮想→物理変換層と CP15。
pub struct Mmu {
    /// c1: 制御
    pub(crate) ctrl: u32,
    /// c2: 変換テーブルベース（bits 31:14）
    pub(crate) ttb: u32,
    /// c3: ドメインアクセス制御
    pub(crate) dacr: u32,
    /// c5: フォルトステータス（データアボートのみ更新）
    pub(crate) fsr: u32,
    /// c6: フォルトアドレス
    pub(crate) far: u32,
    /// c13: FCSE PID（bits 31:25）。VA<32MB を MVA へ再配置する
    pub(crate) pid: u32,
    /// CPU が特権モードか（set_privileged で更新）。AP ビットの権限チェックに使う。
    /// リセット直後は SVC なので true。
    pub(crate) privileged: bool,

    // パイプライン近似: 実機では c1 の M ビットを変えた MCR の時点で
    // 後続 2 命令がフェッチ済みで、WinCE のブートコードは MMU 有効化の
    // 直後の命令が物理アドレスのまま実行されることに依存している。
    // M ビットが変化したら fetch_grace=2 とし、MCR に続く「連続した」命令
    // フェッチをその回数まで変更前の制御レジスタ（prev_ctrl）で変換する。
    // 分岐するとパイプラインはフラッシュされる（分岐先は新状態でフェッチ
    // される）ため、非連続アドレスのフェッチで猶予は打ち切る。
    // データアクセスは即時に新しい状態を使う。
    // TODO: 猶予中に割り込み・例外が入るとベクタフェッチで打ち切られる
    // 挙動になるが、この 2 命令は割り込み禁止で走るのが前提のコード。
    pub(crate) fetch_grace: u32,
    /// 次に猶予が適用される連続アドレス（0 = 最初の 1 回は無条件）
    pub(crate) grace_next: u32,
    pub(crate) prev_ctrl: u32,

    /// その他の crn への書き込みを保持する（読み返し用）。
    pub(crate) regs: [u32; 16],

    /// ソフト TLB。
    pub(crate) tlb: Box<[TlbEntry; TLB_SIZE]>,
    /// 現在の特権状態で見る権限ビット（派生情報: privileged から決まる）。
    perm_r: u8,
    perm_w: u8,

    // 命令フェッチ高速化の派生情報（保存しない。「デコードキャッシュの支援」参照）。
    /// 変換の世代番号
    generation: u64,
    /// デコード済み物理ページの印（1 ビット / 4KB）
    code_pages: Vec<u64>,
    /// CPU が実行中のページの仮想ページ先頭（無効なら 4KB 境界でない値）。
    /// 世代が上がる・コードページに書き込まれると無効にする（Go の SetGenHook の代わり）。
    pub(crate) code_cur_va: u32,
    /// 印の付いたページへの書き込みで、デコード結果を捨てるべき物理ページ
    /// （Go の onCodeWrite の代わり。CPU が次のページ入りで取り出す）。
    code_invalidated: Vec<u32>,
    /// 計測用
    code_marks: u64,
    code_writes: u64,
}

impl Default for Mmu {
    fn default() -> Self {
        Self::new()
    }
}

impl Mmu {
    pub fn new() -> Mmu {
        let mut m = Mmu {
            ctrl: 0,
            ttb: 0,
            dacr: 0,
            fsr: 0,
            far: 0,
            pid: 0,
            privileged: true,
            fetch_grace: 0,
            grace_next: 0,
            prev_ctrl: 0,
            regs: [0; 16],
            tlb: Box::new([EMPTY_ENTRY; TLB_SIZE]),
            perm_r: 0,
            perm_w: 0,
            generation: 0,
            code_pages: vec![0; (1 << 20) / 64],
            code_cur_va: 1,
            code_invalidated: vec![],
            code_marks: 0,
            code_writes: 0,
        };
        m.update_perm_mask();
        m
    }

    /// MMU（CP15 c1 の M ビット）が有効化されているか。
    pub fn enabled(&self) -> bool {
        self.ctrl & CTRL_M != 0
    }

    // ---- 変換テーブルウォーク（ARM ARM B3.4）----

    /// VA→PA 変換と権限チェック。
    fn translate(&self, va: u32, write: bool, phys: &mut impl PhysMem) -> Result<u32, MemError> {
        self.translate_ctrl(self.ctrl, va, write, phys)
    }

    /// 制御レジスタ値を指定した変換。通常は self.ctrl だが、命令フェッチの
    /// 猶予期間（fetch32 参照）だけ変更前の値が渡される。
    fn translate_ctrl(
        &self,
        ctrl: u32,
        va: u32,
        write: bool,
        phys: &mut impl PhysMem,
    ) -> Result<u32, MemError> {
        if ctrl & CTRL_M == 0 {
            return Ok(va);
        }
        // FCSE(c13): VA の下位 32MB は PID で修飾された MVA に再配置される。
        // WinCE はプロセス切替に PID を使う。
        let mva = self.mva(va);

        // TODO: アライメントチェック（A ビット）は未実装。現状 CPU 側が
        // アドレスをアラインしてから発行するため、MMU には非アラインの
        // ワード/ハーフワードアクセスが届かない。非アラインアクセスの
        // フォルト化が必要になったら（ユーザーアプリ実行時など）、
        // CPU がマスク前のアドレスを渡す形に変えて対応する。
        let abort = |status, domain| {
            MemError::Abort(Abort {
                va,
                status,
                domain,
                write,
            })
        };

        // 一次記述子: TTB[31:14] | MVA[31:20] << 2
        let l1_addr = (self.ttb & !0x3FFF) | (mva >> 20) << 2;
        let l1 = phys.read(l1_addr, 4)?;

        match l1 & 3 {
            DESC_SECTION => {
                let domain = ((l1 >> 5) & 0xF) as u8;
                self.check_domain(domain, va, write, FS_DOMAIN_SECT)?;
                if self.domain_client(domain) {
                    let ap = (l1 >> 10) & 3;
                    if !ap_allowed(ctrl, ap, write, self.privileged) {
                        return Err(abort(FS_PERM_SECT, domain));
                    }
                }
                Ok(l1 & 0xFFF00000 | mva & 0x000FFFFF)
            }
            DESC_COARSE | DESC_FINE => {
                let domain = ((l1 >> 5) & 0xF) as u8;
                let l2_addr = if l1 & 3 == DESC_COARSE {
                    // 粗テーブル: ベース[31:10] | MVA[19:12] << 2（256 エントリ）
                    (l1 & !0x3FF) | ((mva >> 12) & 0xFF) << 2
                } else {
                    // 細テーブル: ベース[31:12] | MVA[19:10] << 2（1024 エントリ）
                    (l1 & !0xFFF) | ((mva >> 10) & 0x3FF) << 2
                };
                let l2 = phys.read(l2_addr, 4)?;
                if l2 & 3 == DESC_FAULT {
                    return Err(abort(FS_TRANS_PAGE, domain));
                }
                self.check_domain(domain, va, write, FS_DOMAIN_PAGE)?;
                let (pa, ap) = match l2 & 3 {
                    // 64KB。AP はサブページ（16KB）ごと: MVA[15:14] で選択
                    DESC_LARGE => (
                        l2 & 0xFFFF0000 | mva & 0x0000FFFF,
                        (l2 >> (4 + 2 * ((mva >> 14) & 3))) & 3,
                    ),
                    // 4KB。AP はサブページ（1KB）ごと: MVA[11:10] で選択
                    DESC_SMALL => (
                        l2 & 0xFFFFF000 | mva & 0x00000FFF,
                        (l2 >> (4 + 2 * ((mva >> 10) & 3))) & 3,
                    ),
                    _ => {
                        // 細ページ
                        if l1 & 3 == DESC_COARSE {
                            // 粗テーブル内の tiny 記述子は v4 では無効。
                            // TODO: ARM ARM では UNPREDICTABLE。変換フォルト扱いにしている。
                            return Err(abort(FS_TRANS_PAGE, domain));
                        }
                        (l2 & 0xFFFFFC00 | mva & 0x000003FF, (l2 >> 4) & 3) // 1KB。AP は 1 個
                    }
                };
                if self.domain_client(domain) && !ap_allowed(ctrl, ap, write, self.privileged) {
                    return Err(abort(FS_PERM_PAGE, domain));
                }
                Ok(pa)
            }
            _ => Err(abort(FS_TRANS_SECT, 0)), // フォルト記述子
        }
    }

    /// ドメインがクライアント（AP チェックあり）か。
    /// マネージャ（11）は権限チェックなしでアクセス可。
    fn domain_client(&self, domain: u8) -> bool {
        (self.dacr >> (domain as u32 * 2)) & 3 == 1
    }

    /// ドメインアクセス制御（DACR）を確認する。
    /// 00（no access）と 10（予約）はドメインフォルト。
    fn check_domain(&self, domain: u8, va: u32, write: bool, status: u8) -> Result<(), MemError> {
        match (self.dacr >> (domain as u32 * 2)) & 3 {
            1 | 3 => Ok(()), // client / manager
            // 0: no access, 2: 予約（フォルトに倒す）
            _ => Err(MemError::Abort(Abort {
                va,
                status,
                domain,
                write,
            })),
        }
    }

    // ---- ソフト TLB ----

    fn flush_tlb(&mut self) {
        for e in self.tlb.iter_mut() {
            e.tag = 0;
            e.ram = NO_RAM;
            e.wram = NO_RAM;
            e.watched = false;
        }
        self.bump_gen();
    }

    /// 現在の特権状態に対応する読み/書き権限ビットを選ぶ。
    fn update_perm_mask(&mut self) {
        (self.perm_r, self.perm_w) = if self.privileged {
            (PERM_PRIV_R, PERM_PRIV_W)
        } else {
            (PERM_USER_R, PERM_USER_W)
        };
    }

    /// FCSE 適用後のアドレス。
    #[inline(always)]
    fn mva(&self, va: u32) -> u32 {
        if va < 0x02000000 { va | self.pid } else { va }
    }

    /// TLB を引き、ヒットかつ権限があればエントリの添字を返す。
    /// ミスや権限なしは None（slow path でウォークし、正しいフォルトを作る）。
    #[inline(always)]
    fn lookup(&self, va: u32, need: u8) -> Option<usize> {
        let mva = self.mva(va);
        let i = ((mva >> 12) as usize) & (TLB_SIZE - 1);
        let e = &self.tlb[i];
        (e.tag == mva >> 12 | TLB_VALID && e.perm & need != 0).then_some(i)
    }

    /// slow path で変換が成功した後に、そのページのエントリを作る。
    /// ページ全体の権限を 4 通り求めるため、ウォークをもう一度行う
    /// （ミス時だけなのでコストは問題にならない）。
    fn fill(&mut self, va: u32, phys: &mut impl PhysMem) {
        let mva = self.mva(va);
        let Some((pa, perm)) = self.page_info(mva, phys) else {
            return;
        };
        let ram = phys.ram_page(pa).unwrap_or(NO_RAM);
        let wram = if self.is_code(pa) { NO_RAM } else { ram };
        let e = &mut self.tlb[((mva >> 12) as usize) & (TLB_SIZE - 1)];
        let watched = e.watched;
        *e = TlbEntry {
            tag: mva >> 12 | TLB_VALID,
            pa,
            perm,
            ram,
            wram,
            watched: false,
        };
        if watched {
            // CPU が覚えているページのエントリを追い出した（「デコードキャッシュの支援」）。
            self.bump_gen();
        }
    }

    /// MVA を含む 4KB ページの物理先頭と、4 通りの権限を求める。
    /// 4KB 内で変換・権限が一様でないマッピングやフォルトは None。
    fn page_info(&self, mva: u32, phys: &mut impl PhysMem) -> Option<(u32, u8)> {
        let ctrl = self.ctrl;
        if ctrl & CTRL_M == 0 {
            return Some((mva & !0xFFF, PERM_ALL));
        }
        let l1 = phys.read((self.ttb & !0x3FFF) | (mva >> 20) << 2, 4).ok()?;
        let domain = (l1 >> 5) & 0xF;
        let (pa, ap) = match l1 & 3 {
            DESC_SECTION => (l1 & 0xFFF00000 | mva & 0x000FF000, (l1 >> 10) & 3),
            DESC_COARSE => {
                let l2 = phys
                    .read((l1 & !0x3FF) | ((mva >> 12) & 0xFF) << 2, 4)
                    .ok()?;
                match l2 & 3 {
                    DESC_LARGE => (
                        l2 & 0xFFFF0000 | mva & 0x0000F000,
                        (l2 >> (4 + 2 * ((mva >> 14) & 3))) & 3,
                    ),
                    DESC_SMALL => {
                        // 4 サブページの AP が揃っているときだけキャッシュする。
                        let aps = (l2 >> 4) & 0xFF;
                        let ap = aps & 3;
                        if aps != ap * 0x55 {
                            return None;
                        }
                        (l2 & 0xFFFFF000, ap)
                    }
                    _ => return None,
                }
            }
            // 細テーブル（tiny ページ 1KB を含み得る）とフォルトはキャッシュしない。
            _ => return None,
        };
        match (self.dacr >> (domain * 2)) & 3 {
            3 => Some((pa, PERM_ALL)), // マネージャ: 権限チェックなし
            1 => {
                // クライアント: AP に従う
                let mut perm = 0;
                for (privileged, write, bit) in [
                    (true, false, PERM_PRIV_R),
                    (true, true, PERM_PRIV_W),
                    (false, false, PERM_USER_R),
                    (false, true, PERM_USER_W),
                ] {
                    if ap_allowed(ctrl, ap, write, privileged) {
                        perm |= bit;
                    }
                }
                Some((pa, perm))
            }
            _ => None, // ドメインフォルト: キャッシュしない
        }
    }

    /// slow path の変換。成功したら TLB に載せる。
    fn translate_fill(
        &mut self,
        va: u32,
        write: bool,
        phys: &mut impl PhysMem,
    ) -> Result<u32, MemError> {
        let pa = self.translate(va, write, phys)?;
        self.fill(va, phys);
        Ok(pa)
    }

    // ---- fast path 付きアクセス ----
    // アラインされていないアクセスは slow path に任せる（ページ境界跨ぎや
    // 端数の扱いを bus と同一にするため。CPU は通常アライン済みで発行する）。

    /// データの読み出し（size は 1/2/4）。
    #[inline(always)]
    pub fn read(&mut self, va: u32, size: u32, phys: &mut impl PhysMem) -> Result<u32, MemError> {
        if va & (size - 1) == 0
            && let Some(i) = self.lookup(va, self.perm_r)
        {
            let e = self.tlb[i];
            if e.ram != NO_RAM {
                let a = (e.ram + (va & 0xFFF)) as usize;
                let m = phys.arena();
                return Ok(match size {
                    1 => m[a] as u32,
                    2 => u16::from_le_bytes([m[a], m[a + 1]]) as u32,
                    _ => u32::from_le_bytes([m[a], m[a + 1], m[a + 2], m[a + 3]]),
                });
            }
            return Ok(phys.read(e.pa | va & 0xFFF, size)?);
        }
        self.read_slow(va, size, phys)
    }

    #[inline(never)]
    fn read_slow(&mut self, va: u32, size: u32, phys: &mut impl PhysMem) -> Result<u32, MemError> {
        let pa = self.translate_fill(va, false, phys)?;
        Ok(phys.read(pa, size)?)
    }

    /// データの書き込み（size は 1/2/4。v の下位 size バイト）。
    #[inline(always)]
    pub fn write(
        &mut self,
        va: u32,
        size: u32,
        v: u32,
        phys: &mut impl PhysMem,
    ) -> Result<(), MemError> {
        if va & (size - 1) == 0
            && let Some(i) = self.lookup(va, self.perm_w)
        {
            let e = self.tlb[i];
            if e.wram != NO_RAM {
                let a = (e.wram + (va & 0xFFF)) as usize;
                phys.arena_mut()[a..a + size as usize]
                    .copy_from_slice(&v.to_le_bytes()[..size as usize]);
                return Ok(());
            }
            self.check_code_write(e.pa);
            return Ok(phys.write(e.pa | va & 0xFFF, size, v)?);
        }
        self.write_slow(va, size, v, phys)
    }

    #[inline(never)]
    fn write_slow(
        &mut self,
        va: u32,
        size: u32,
        v: u32,
        phys: &mut impl PhysMem,
    ) -> Result<(), MemError> {
        let pa = self.translate_fill(va, true, phys)?;
        self.check_code_write(pa);
        Ok(phys.write(pa, size, v)?)
    }

    /// 命令フェッチ。M ビット変更直後、MCR に続く連続した最大 2 命令だけ
    /// 変更前の変換状態を使う（パイプライン近似。構造体のコメント参照）。
    /// 猶予中は TLB を引かず、埋めもしない（Go と同じ）。
    #[inline(always)]
    pub fn fetch32(&mut self, va: u32, phys: &mut impl PhysMem) -> Result<u32, MemError> {
        if self.fetch_grace == 0 {
            return self.read(va, 4, phys); // 通常は TLB 付きのデータ読み出しと同じ経路
        }
        self.fetch32_grace(va, phys)
    }

    #[inline(never)]
    fn fetch32_grace(&mut self, va: u32, phys: &mut impl PhysMem) -> Result<u32, MemError> {
        let mut ctrl = self.ctrl;
        if self.grace_next == 0 || va == self.grace_next {
            self.fetch_grace -= 1;
            self.grace_next = va.wrapping_add(4);
            ctrl = self.prev_ctrl;
        } else {
            self.fetch_grace = 0; // 分岐した: パイプラインフラッシュ相当
        }
        let pa = self.translate_ctrl(ctrl, va, false, phys)?;
        Ok(phys.read(pa, 4)?)
    }

    /// TLB にヒットし（= 実際のアクセスでも TLB が変わらない）、かつ読み出しに
    /// 副作用が無い場合だけ値を返す（CPU のアイドルループ検出用。Go の Probe32）:
    ///   - RAM ページ: 実体を直接読む。監視中は RAM の実体を持たないので
    ///     下の MMIO と同じ扱いになり、bus 側が監視中は不可と答える。
    ///   - それ以外（MMIO）: データの読み出しに限り、物理空間の probe32 に問う
    ///     （ポーリングされるステータスレジスタ。命令フェッチは RAM だけ）。
    ///
    /// フェッチはフェッチ猶予中なら不可（猶予の残り回数が変わるため）。
    pub fn probe32(&mut self, va: u32, fetch: bool, phys: &mut impl PhysMem) -> Option<u32> {
        if va & 3 != 0 || (fetch && self.fetch_grace != 0) {
            return None;
        }
        let e = self.tlb[self.lookup(va, self.perm_r)?];
        if e.ram != NO_RAM {
            let a = (e.ram + (va & 0xFFF)) as usize;
            let m = phys.arena();
            return Some(u32::from_le_bytes([m[a], m[a + 1], m[a + 2], m[a + 3]]));
        }
        if fetch {
            return None;
        }
        phys.probe32(e.pa | va & 0xFFF)
    }

    /// 現在の変換状態で VA を PA に変換する（デバッグ用）。権限チェックは
    /// 読み出しとして行う。MMU 無効なら VA をそのまま返す。テーブルウォークは
    /// 物理バスを読むだけで、MMU の状態は変えない。
    pub fn translate_debug(&self, va: u32, phys: &mut impl PhysMem) -> Result<u32, MemError> {
        self.translate(va, false, phys)
    }

    // ---- CP15 ----

    /// MRC p15。
    pub fn cp15_read(&self, _opc1: u8, crn: u8, _crm: u8, opc2: u8) -> u32 {
        match crn {
            0 if opc2 == 1 => ARM920_CACHE_TYPE,
            0 => ARM920_MAIN_ID,
            1 => self.ctrl,
            2 => self.ttb,
            3 => self.dacr,
            5 => self.fsr,
            6 => self.far,
            13 => self.pid,
            _ => self.regs[(crn & 15) as usize],
        }
    }

    /// MCR p15。
    pub fn cp15_write(&mut self, _opc1: u8, crn: u8, _crm: u8, _opc2: u8, v: u32) {
        match crn {
            1 => {
                if (self.ctrl ^ v) & CTRL_M != 0 {
                    // M ビットが変わる: 後続の連続 2 命令のフェッチは旧状態で行う。
                    self.prev_ctrl = self.ctrl;
                    self.fetch_grace = 2;
                    self.grace_next = 0;
                }
                self.ctrl = v;
                self.flush_tlb();
            }
            2 => {
                self.ttb = v;
                self.flush_tlb();
            }
            3 => {
                self.dacr = v;
                self.flush_tlb();
            }
            5 => self.fsr = v, // OS がコンテキスト復元で書くことがある
            6 => self.far = v,
            // キャッシュ操作（wait-for-interrupt の c7,c0,4 を含む）: no-op。
            // TODO: wait-for-interrupt を「割り込みまで停止」に最適化すると
            // アイドルループが速くなる。当面はビジーループで正しく動く。
            7 => {}
            // TLB 操作: 単一エントリ指定も含めて全無効化する（安全側）。
            8 => self.flush_tlb(),
            13 => {
                self.pid = v & 0xFE000000;
                self.bump_gen(); // VA<32MB の MVA が変わる
            }
            _ => self.regs[(crn & 15) as usize] = v,
        }
    }

    /// 例外ベクタのベース（V ビットで 0xFFFF0000 / 0）。
    pub fn vector_base(&self) -> u32 {
        if self.ctrl & CTRL_V != 0 {
            0xFFFF0000
        } else {
            0
        }
    }

    /// CPU の特権状態の変化を受ける（usr だけが非特権）。
    pub fn set_privileged(&mut self, privileged: bool) {
        if self.privileged != privileged {
            self.bump_gen(); // フェッチの権限判定が変わる
        }
        self.privileged = privileged;
        self.update_perm_mask();
    }

    /// データアボートの FSR/FAR の更新（CPU が例外に入るときに呼ぶ）。
    pub fn record_data_abort(&mut self, a: &Abort) {
        self.fsr = (a.domain as u32) << 4 | a.status as u32;
        self.far = a.va;
    }
}

// ---- デコードキャッシュの支援（性能対策。ユーザー確認済み 2026-09。Go の code.go）----
//
// CPU（arm）は物理 RAM ページ単位でデコード済みの命令を持ち、同じページを
// 実行している間は TLB を引かずにそこから命令を取る。MMU は次を提供する:
//
//   - code_page: 実行を始めるページの変換（TLB ヒットのときだけ。ミスなら CPU は
//     通常のフェッチでこの命令を取り、TLB が埋まる。TLB の状態の変化は
//     1 命令ずつ fetch32 していた場合と同一になる）。
//   - 世代（gen）: 「CPU が覚えたページの変換がまだ有効か」の番号。変換・権限・
//     TLB の中身が変わり得る操作（TLB の全無効化、特権状態・FCSE PID の変化、
//     CPU が覚えているエントリの詰め替え）で増やし、code_cur_va を無効にする
//     （CPU は実行中のページの記憶を捨てるので、毎命令の比較が要らない）。
//     詰め替えで世代を上げるのは、コードページの TLB エントリがデータアクセスで
//     追い出された場合に、CPU が次のフェッチで元どおり TLB を埋め直すため
//     （TLB の状態を通常のフェッチと一致させる）。code_page で渡したエントリに
//     watched の印を付け、そのエントリの詰め替えだけで世代を上げる。
//   - コードページの書き込み検出（書き込み保護方式）: CPU がデコードした
//     物理ページは code_pages に印を付け、そのページを指す TLB エントリの
//     wram（直接書き込み用の実体）を外す。そのページへのストアだけ遅い経路に
//     回り、そこで CPU に無効化を知らせてから書く（code_invalidated に積み、
//     code_cur_va を無効にするので、CPU は次の命令でページに入り直すときに
//     デコード結果を捨てる）。普通のストアには追加の判定が入らない。同じ物理
//     ページを別の仮想アドレスが指していても物理ページ単位で管理するので漏れない。
//
// code_pages・wram・世代・watched は、デコードキャッシュと同じく実行を速くする
// ための派生情報で、スナップショットには保存しない（復元時は空から作り直す）。
// どれも TLB の tag/pa/perm（ゲストから見える状態）を変えない。

impl Mmu {
    /// 命令フェッチ用に va を含むページの変換を TLB から引く。TLB ヒットかつ RAM で、
    /// フェッチ猶予中でないときだけ (物理ページ先頭, RAM の位置) を返す。
    /// TLB の tag/pa/perm は変えない（watched の印だけ付ける）。
    pub fn code_page(&mut self, va: u32) -> Option<(u32, RamOff)> {
        if self.fetch_grace != 0 {
            return None;
        }
        let i = self.lookup(va, self.perm_r)?;
        let e = &mut self.tlb[i];
        if e.ram == NO_RAM {
            return None;
        }
        e.watched = true;
        Some((e.pa, e.ram))
    }

    /// va から nbytes バイト（同じ 4KB ページ内）を、CPU が直接読み書きできる
    /// RAM の範囲の先頭位置として返す（LDM/STM の高速化用）。TLB ヒットで権限が
    /// あり、読み出しなら RAM、書き込みなら直接書き込み可（コードページでない RAM）の
    /// ときだけ Some。状態は変えない（同じページへの連続アクセスがすべて TLB
    /// ヒットになる場合と同じ）。
    pub fn ram_run(&self, va: u32, nbytes: u32, write: bool) -> Option<RamOff> {
        let off = va & 0xFFF;
        if va & 3 != 0 || nbytes == 0 || off + nbytes > 0x1000 {
            return None;
        }
        let e = &self.tlb[self.lookup(va, if write { self.perm_w } else { self.perm_r })?];
        let ram = if write { e.wram } else { e.ram };
        (ram != NO_RAM).then_some(ram + off)
    }

    /// 変換の世代番号。
    pub fn code_gen(&self) -> u64 {
        self.generation
    }

    /// 世代を上げ、CPU の実行中ページの記憶を無効にする。
    fn bump_gen(&mut self) {
        self.generation += 1;
        self.code_cur_va = 1;
    }

    /// 物理ページ pa（4KB 境界）をデコード済み（コード）として印を付け、以後
    /// そのページへの書き込みを検出できるようにする。
    pub fn mark_code(&mut self, pa: u32) {
        let pn = (pa >> 12) as usize;
        if self.code_pages[pn >> 6] & (1 << (pn & 63)) != 0 {
            return;
        }
        self.code_pages[pn >> 6] |= 1 << (pn & 63);
        self.code_marks += 1;
        for e in self.tlb.iter_mut() {
            if e.tag & TLB_VALID != 0 && e.pa == pa {
                e.wram = NO_RAM;
            }
        }
    }

    /// 物理アドレスを含むページに印があるか。
    fn is_code(&self, pa: u32) -> bool {
        let pn = (pa >> 12) as usize;
        self.code_pages[pn >> 6] & (1 << (pn & 63)) != 0
    }

    /// 印の付いたページへの書き込みの直前に呼ぶ。印を外して直接書き込みを戻し、
    /// CPU にデコード結果を捨てさせる。
    fn code_write(&mut self, pa: u32) {
        let page = pa & !0xFFF;
        let pn = (page >> 12) as usize;
        self.code_pages[pn >> 6] &= !(1 << (pn & 63));
        self.code_writes += 1;
        for e in self.tlb.iter_mut() {
            if e.tag & TLB_VALID != 0 && e.pa == page {
                e.wram = e.ram;
            }
        }
        self.code_invalidated.push(page);
        self.code_cur_va = 1;
    }

    /// 遅い経路の書き込み（TLB ミス・MMIO・監視中）で、書き込み先がコードページなら
    /// 知らせる。
    #[inline(always)]
    fn check_code_write(&mut self, pa: u32) {
        if self.is_code(pa) {
            self.code_write(pa);
        }
    }

    /// デコード結果を捨てるべき物理ページを取り出す（CPU がページに入るときに呼ぶ）。
    pub fn take_code_invalidated(&mut self) -> Option<u32> {
        self.code_invalidated.pop()
    }

    /// コードページの印を全部外す（リセット・スナップショット復元時など。
    /// CPU 側もデコードキャッシュを捨てる前提）。
    pub fn reset_code(&mut self) {
        self.code_pages.iter_mut().for_each(|w| *w = 0);
        self.code_invalidated.clear();
        for e in self.tlb.iter_mut() {
            e.wram = e.ram;
        }
        self.bump_gen();
    }

    /// コードページの印付け・書き込み検出の回数（性能調査用）。
    pub fn code_stats(&self) -> (u64, u64) {
        (self.code_marks, self.code_writes)
    }
}

/// AP ビットと S/R ビットによるアクセス可否（ARM ARM B3-16）。
fn ap_allowed(ctrl: u32, ap: u32, write: bool, privileged: bool) -> bool {
    match ap {
        0 => {
            // AP=00 は S/R ビット次第の読み出し専用空間。
            if write {
                false
            } else if ctrl & CTRL_R != 0 {
                true // R=1: 全モード読み出し可
            } else if ctrl & CTRL_S != 0 {
                privileged // S=1: 特権のみ読み出し可
            } else {
                false
            }
        }
        1 => privileged,           // 特権のみ RW
        2 => privileged || !write, // 特権 RW / ユーザー読み出しのみ
        _ => true,                 // 3: 全モード RW
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::{Bus, BusError, BusPhys, Devices, NoDevices};

    // テストは物理空間として実物の bus を使う（RAM 2MB、アドレス 0 起点）。
    // L1 テーブルは TEST_TTB（16KB アライン）、L2 テーブルは TEST_L2 に置く。
    const TEST_TTB: u32 = 0x8000;
    const TEST_L2: u32 = 0xC000;

    struct T {
        m: Mmu,
        b: Bus<u8>,
        d: Count,
    }

    /// MMIO の読み出し回数を数えるデバイス（読み値はオフセット）。
    #[derive(Default)]
    struct Count {
        reads: u32,
    }

    impl Devices<u8> for Count {
        fn read(&mut self, _: u8, off: u32, _: u32) -> u32 {
            self.reads += 1;
            off
        }
        fn write(&mut self, _: u8, _: u32, _: u32, _: u32) {}
        fn stable_read(&mut self, _: u8, _: u32, _: u32) -> Option<u32> {
            None
        }
    }

    impl T {
        fn new(ram: u32) -> T {
            let mut b = Bus::new();
            b.map_ram("ram", 0, ram).unwrap();
            T {
                m: Mmu::new(),
                b,
                d: Count::default(),
            }
        }
        /// setupMMU: TTB・DACR（ドメイン 0 = クライアント）・MMU 有効。
        fn setup() -> T {
            let mut t = T::new(2 << 20);
            t.m.cp15_write(0, 2, 0, 0, TEST_TTB);
            t.m.cp15_write(0, 3, 0, 0, 1);
            t.m.cp15_write(0, 1, 0, 0, CTRL_M);
            t
        }
        fn p(&mut self) -> BusPhys<'_, u8, Count> {
            BusPhys {
                bus: &mut self.b,
                devs: &mut self.d,
            }
        }
        fn read(&mut self, va: u32, size: u32) -> Result<u32, MemError> {
            let mut p = BusPhys {
                bus: &mut self.b,
                devs: &mut self.d,
            };
            self.m.read(va, size, &mut p)
        }
        fn write(&mut self, va: u32, size: u32, v: u32) -> Result<(), MemError> {
            let mut p = BusPhys {
                bus: &mut self.b,
                devs: &mut self.d,
            };
            self.m.write(va, size, v, &mut p)
        }
        fn fetch(&mut self, va: u32) -> Result<u32, MemError> {
            let mut p = BusPhys {
                bus: &mut self.b,
                devs: &mut self.d,
            };
            self.m.fetch32(va, &mut p)
        }
        fn phys_w(&mut self, pa: u32, v: u32) {
            self.p().write(pa, 4, v).unwrap();
        }
        fn phys_r(&mut self, pa: u32) -> u32 {
            self.p().read(pa, 4).unwrap()
        }
        /// va に対応する一次エントリを書く。
        fn set_l1(&mut self, va: u32, desc: u32) {
            self.phys_w(TEST_TTB + (va >> 20) * 4, desc);
        }
    }

    /// 一次セクション記述子。
    fn section(pa: u32, domain: u32, ap: u32) -> u32 {
        pa & 0xFFF00000 | ap << 10 | domain << 5 | 2
    }

    /// 二次小ページ記述子（4 サブページとも同じ AP）。
    fn small(pa: u32, ap: u32) -> u32 {
        pa & 0xFFFFF000 | ap << 10 | ap << 8 | ap << 6 | ap << 4 | 2
    }

    #[track_caller]
    fn want_abort<T: std::fmt::Debug>(r: Result<T, MemError>, status: u8, domain: u8, write: bool) {
        match r {
            Err(MemError::Abort(a)) => {
                assert_eq!(
                    (a.status, a.domain, a.write),
                    (status, domain, write),
                    "{a:?}"
                )
            }
            other => panic!("got {other:?}, want abort"),
        }
    }

    #[test]
    fn disabled_passthrough() {
        let mut t = T::new(0x1000);
        t.write(0x100, 4, 0xCAFEF00D).unwrap();
        assert_eq!(t.phys_r(0x100), 0xCAFEF00D);
    }

    #[test]
    fn section_translation() {
        let mut t = T::setup();
        t.set_l1(0x80100000, section(0x00100000, 0, 3));
        t.write(0x80112344, 4, 0xDEADBEEF).unwrap();
        assert_eq!(t.phys_r(0x00112344), 0xDEADBEEF);
        assert_eq!(t.read(0x80112344, 4), Ok(0xDEADBEEF));
        assert_eq!(t.read(0x80112345, 1), Ok(0xBE));
    }

    #[test]
    fn section_translation_fault() {
        let mut t = T::setup(); // L1 は全部 0 = フォルト記述子
        want_abort(t.read(0x80000000, 4), FS_TRANS_SECT, 0, false);
        want_abort(t.write(0x80000000, 4, 1), FS_TRANS_SECT, 0, true);
    }

    #[test]
    fn domain_fault() {
        let mut t = T::setup();
        t.set_l1(0x80100000, section(0x00100000, 3, 3)); // ドメイン3
        // DACR はドメイン0 のみ設定済み → ドメイン3 は 00 (no access)
        want_abort(t.read(0x80100000, 4), FS_DOMAIN_SECT, 3, false);
    }

    #[test]
    fn manager_domain_skips_ap() {
        let mut t = T::setup();
        t.set_l1(0x80100000, section(0x00100000, 2, 0)); // AP=00（アクセス不可相当）
        t.m.cp15_write(0, 3, 0, 0, 3 << 4); // ドメイン2 = マネージャ
        t.write(0x80100000, 4, 1).unwrap();
    }

    #[test]
    fn section_permissions() {
        // (名前, AP, CTRL_M に足す S/R, 特権, 書き込み, フォルトか)
        let cases = [
            ("AP=01 特権RW", 1, 0, true, true, false),
            ("AP=01 ユーザー読み不可", 1, 0, false, false, true),
            ("AP=10 特権RW", 2, 0, true, true, false),
            ("AP=10 ユーザー読みOK", 2, 0, false, false, false),
            ("AP=10 ユーザー書き不可", 2, 0, false, true, true),
            ("AP=11 ユーザーRW", 3, 0, false, true, false),
            ("AP=00 S=0,R=0 特権読みも不可", 0, 0, true, false, true),
            ("AP=00 S=1 特権読みOK", 0, CTRL_S, true, false, false),
            ("AP=00 S=1 特権書き不可", 0, CTRL_S, true, true, true),
            ("AP=00 S=1 ユーザー読み不可", 0, CTRL_S, false, false, true),
            ("AP=00 R=1 ユーザー読みOK", 0, CTRL_R, false, false, false),
            ("AP=00 R=1 書き不可", 0, CTRL_R, true, true, true),
        ];
        for (name, ap, ctrl, privileged, write, fault) in cases {
            let mut t = T::setup();
            t.set_l1(0x80100000, section(0x00100000, 0, ap));
            t.m.cp15_write(0, 1, 0, 0, CTRL_M | ctrl);
            t.m.set_privileged(privileged);
            let r = if write {
                t.write(0x80100000, 4, 1)
            } else {
                t.read(0x80100000, 4).map(|_| ())
            };
            if fault {
                want_abort(r, FS_PERM_SECT, 0, write);
            } else {
                assert!(r.is_ok(), "{name}: {r:?}");
            }
        }
    }

    #[test]
    fn coarse_small_page() {
        let mut t = T::setup();
        // VA 0x80100000〜 を粗テーブル経由で 4KB ページにマップ。
        t.set_l1(0x80100000, TEST_L2 | DESC_COARSE);
        // idx0: PA 0x50000、idx1: フォルト（未設定）
        t.phys_w(TEST_L2, small(0x00050000, 3));
        // 非アラインは slow path で変換し、バスがそのまま扱う（Go と同じ）
        t.write(0x80100123, 4, 0x12345678).unwrap();
        assert_eq!(t.phys_r(0x00050123), 0x12345678);
        // 隣の 4KB はページ変換フォルト
        want_abort(t.read(0x80101000, 4), FS_TRANS_PAGE, 0, false);
    }

    #[test]
    fn small_page_subpage_ap() {
        let mut t = T::setup();
        t.set_l1(0x80100000, TEST_L2 | DESC_COARSE);
        // ap0（オフセット 0x000-0x3FF）= 3（全モードRW）、ap1（0x400-0x7FF）= 1（特権のみ）
        t.phys_w(TEST_L2, 0x00050000 | 1 << 6 | 3 << 4 | 2);
        t.m.set_privileged(false);
        t.read(0x80100000, 4).unwrap();
        want_abort(t.read(0x80100400, 4), FS_PERM_PAGE, 0, false);
    }

    #[test]
    fn large_page() {
        let mut t = T::setup();
        t.set_l1(0x80100000, TEST_L2 | DESC_COARSE);
        // VA 0x8010C123 → L2 idx = 0xC。大ページ PA 0x00060000。
        // AP サブフィールドは MVA[15:14]=3 → ap3（bits 11:10）だけ 3 にする。
        t.phys_w(TEST_L2 + 0xC * 4, 0x00060000 | 3 << 10 | DESC_LARGE);
        t.write(0x8010C120, 4, 0xA5A5A5A5).unwrap();
        assert_eq!(
            t.phys_r(0x0006C120),
            0xA5A5A5A5,
            "64KB ページ内オフセット維持"
        );
    }

    #[test]
    fn fine_tiny_page() {
        let mut t = T::setup();
        // 細テーブル（4KB アライン、1KB ページ）
        t.set_l1(0x80100000, TEST_L2 | DESC_FINE);
        // VA 0x80100800 → idx = (0x800>>10) = 2
        t.phys_w(TEST_L2 + 2 * 4, 0x00070000 | 3 << 4 | 3);
        t.write(0x80100823, 4, 0x77).unwrap();
        // 1KB ページなので PA = 0x70000 + (VA & 0x3FF)
        assert_eq!(t.phys_r(0x00070023), 0x77);
    }

    #[test]
    fn fcse_remap() {
        let mut t = T::setup();
        t.m.cp15_write(0, 13, 0, 0, 1 << 25); // PID = 1 → VA<32MB は MVA 0x02000000〜
        t.set_l1(0x02000000, section(0x00100000, 0, 3));
        t.write(0x00001234, 4, 0xBEEF).unwrap();
        assert_eq!(t.phys_r(0x00101234), 0xBEEF);
        // 32MB 以上の VA は PID の影響を受けない
        t.set_l1(0x80100000, section(0x00000000, 0, 3));
        t.read(0x80100000, 4).unwrap();
    }

    #[test]
    fn l1_walk_bus_error_stops() {
        // TTB が未マップ物理を指す場合はアボートではなく停止用エラー。
        let mut t = T::new(0x1000);
        t.m.cp15_write(0, 2, 0, 0, 0x100000); // RAM 外
        t.m.cp15_write(0, 1, 0, 0, CTRL_M);
        assert_eq!(
            t.read(0x80000000, 4),
            Err(MemError::Bus(BusError {
                addr: 0x102000,
                write: false
            }))
        );
    }

    #[test]
    fn vector_base() {
        let mut t = T::setup();
        assert_eq!(t.m.vector_base(), 0);
        t.m.cp15_write(0, 1, 0, 0, CTRL_M | CTRL_V);
        assert_eq!(t.m.vector_base(), 0xFFFF0000);
    }

    // ---- TLB（Go の tlb_test.go）----

    /// TLB は c8 の無効化まで古い変換を保持する（実機と同じ）。
    #[test]
    fn tlb_stale_until_flush() {
        let mut t = T::setup();
        t.phys_w(0x00100000, 0xAAAA);
        t.phys_w(0x00000000, 0xBBBB);
        t.set_l1(0x80100000, section(0x00100000, 0, 3));
        assert_eq!(t.read(0x80100000, 4), Ok(0xAAAA));
        t.set_l1(0x80100000, section(0x00000000, 0, 3)); // 無効化なしで張り替え
        assert_eq!(t.read(0x80100000, 4), Ok(0xAAAA), "before c8: want stale");
        t.m.cp15_write(0, 8, 0, 0, 0);
        assert_eq!(t.read(0x80100000, 4), Ok(0xBBBB));
    }

    /// フォルトはキャッシュされないので、新規マッピングは無効化なしで見える。
    #[test]
    fn tlb_does_not_cache_faults() {
        let mut t = T::setup();
        assert!(t.read(0x80100000, 4).is_err());
        t.set_l1(0x80100000, section(0x00100000, 0, 3));
        t.read(0x80100000, 4).unwrap();
    }

    /// 特権で載せたエントリでも、ユーザーモードの権限チェックは効く。
    #[test]
    fn tlb_permission_per_mode() {
        let mut t = T::setup();
        t.set_l1(0x80100000, section(0x00100000, 0, 1)); // AP=01: 特権のみ
        t.write(0x80100000, 4, 1).unwrap();
        t.m.set_privileged(false);
        want_abort(t.read(0x80100000, 4), FS_PERM_SECT, 0, false);
        t.m.set_privileged(true);
        t.read(0x80100000, 4).unwrap();
    }

    /// DACR 変更（c3）で無効化され、ドメインフォルトが即座に効く。
    #[test]
    fn tlb_flush_on_dacr() {
        let mut t = T::setup();
        t.set_l1(0x80100000, section(0x00100000, 0, 3));
        t.read(0x80100000, 4).unwrap();
        t.m.cp15_write(0, 3, 0, 0, 0); // ドメイン0 = no access
        want_abort(t.read(0x80100000, 4), FS_DOMAIN_SECT, 0, false);
    }

    /// FCSE: PID が変わると同じ VA でも別の MVA として引かれる（無効化不要）。
    #[test]
    fn tlb_fcse_tag() {
        let mut t = T::setup();
        t.phys_w(0x00100000, 1);
        t.phys_w(0x00000000, 2);
        t.set_l1(0x02000000, section(0x00100000, 0, 3)); // PID1 の slot
        t.set_l1(0x04000000, section(0x00000000, 0, 3)); // PID2 の slot
        t.m.cp15_write(0, 13, 0, 0, 1 << 25);
        assert_eq!(t.read(0, 4), Ok(1));
        t.m.cp15_write(0, 13, 0, 0, 2 << 25);
        assert_eq!(t.read(0, 4), Ok(2));
    }

    /// MMIO ページは TLB ヒットしても毎回デバイスに届く（RAM 直アクセスしない）。
    #[test]
    fn tlb_mmio_goes_to_device() {
        let mut t = T::setup();
        t.b.map_mmio("dev", 0x4D000000, 0x1000, 0).unwrap();
        t.set_l1(0x90D00000, section(0x4D000000, 0, 1));
        for _ in 0..3 {
            assert_eq!(t.read(0x90D00010, 4), Ok(0x10));
        }
        assert_eq!(t.d.reads, 3);
    }

    /// TLB に載せる・載せないの規則（Go の fill/pageInfo と同じ）。
    #[test]
    fn tlb_fill_rules() {
        let mut t = T::setup();
        let slot = |va: u32| ((va >> 12) as usize) & (TLB_SIZE - 1);
        // セクション: 載る
        t.set_l1(0x80100000, section(0x00100000, 0, 3));
        t.read(0x80100010, 4).unwrap();
        let e = t.m.tlb[slot(0x80100000)];
        assert_eq!(
            (e.tag, e.pa, e.perm),
            (0x80100 | TLB_VALID, 0x00100000, PERM_ALL)
        );
        assert_eq!(e.ram, 0x00100000, "RAM ページはアリーナの位置を持つ");
        // サブページの AP が揃わない小ページ: 載らない
        t.set_l1(0x80200000, TEST_L2 | DESC_COARSE);
        t.phys_w(TEST_L2, 0x00050000 | 1 << 6 | 3 << 4 | 2);
        t.read(0x80200000, 4).unwrap();
        assert_eq!(t.m.tlb[slot(0x80200000)].tag, 0);
        // 細テーブル: 載らない
        t.set_l1(0x80300000, 0xD000 | DESC_FINE);
        t.phys_w(0xD000, 0x00070000 | 3 << 4 | 3);
        t.read(0x80300000, 4).unwrap();
        assert_eq!(t.m.tlb[slot(0x80300000)].tag, 0);
        // クライアントの AP=10: 特権 RW・ユーザー R
        t.set_l1(0x80400000, section(0x00100000, 0, 2));
        t.read(0x80400000, 4).unwrap();
        assert_eq!(
            t.m.tlb[slot(0x80400000)].perm,
            PERM_PRIV_R | PERM_PRIV_W | PERM_USER_R
        );
        // MMU 無効: 恒等変換を全権限で載せる
        let mut u = T::new(0x2000);
        u.read(0x1004, 4).unwrap();
        let e = u.m.tlb[1];
        assert_eq!((e.tag, e.pa, e.perm), (1 | TLB_VALID, 0x1000, PERM_ALL));
        // c13 では無効化しない、c1 では無効化する
        t.m.cp15_write(0, 13, 0, 0, 1 << 25);
        assert_ne!(t.m.tlb[slot(0x80100000)].tag, 0);
        t.m.cp15_write(0, 1, 0, 0, CTRL_M);
        assert_eq!(t.m.tlb[slot(0x80100000)].tag, 0);
    }

    // ---- フェッチ猶予（Go の grace_test.go）----
    // 実イメージのブートコードが依存するイディオム:
    //
    //   mcr p15, 0, r1, c1, c0, 0  ; MMU 有効化
    //   mov pc, r0                 ; ← 物理のままフェッチ済みで実行される
    //   （分岐先からは変換が効く）
    fn grace() -> T {
        let mut t = T::new(2 << 20);
        t.m.cp15_write(0, 2, 0, 0, TEST_TTB);
        t.m.cp15_write(0, 3, 0, 0, 1);
        // 物理 0x100/0x104 に目印（ページテーブルは全フォルトのまま）
        t.phys_w(0x100, 0xAAAAAAAA);
        t.phys_w(0x104, 0xAAAAAAAA);
        t.m.cp15_write(0, 1, 0, 0, CTRL_M); // 有効化 → 連続 2 フェッチの猶予
        t
    }

    #[test]
    fn fetch_grace_sequential() {
        let mut t = grace();
        // 連続した 2 命令は物理のまま
        assert_eq!(t.fetch(0x100), Ok(0xAAAAAAAA));
        assert_eq!(t.fetch(0x104), Ok(0xAAAAAAAA));
        // 3 命令目からは変換される（未マップなので変換フォルト）
        want_abort(t.fetch(0x108), FS_TRANS_SECT, 0, false);
    }

    #[test]
    fn fetch_grace_ends_on_branch() {
        let mut t = grace();
        t.fetch(0x100).unwrap();
        // 非連続アドレス（分岐先）はパイプラインフラッシュ相当で新状態
        want_abort(t.fetch(0x200), FS_TRANS_SECT, 0, false);
        assert_eq!(t.m.fetch_grace, 0);
    }

    #[test]
    fn data_read_ignores_grace() {
        let mut t = grace();
        // データリードは猶予に関係なく即時に新状態
        want_abort(t.read(0x100, 4), FS_TRANS_SECT, 0, false);
    }

    #[test]
    fn probe32() {
        let mut t = T::setup();
        t.set_l1(0x80100000, section(0x00100000, 0, 3));
        t.phys_w(0x00100010, 0x1234);
        let mut p = BusPhys {
            bus: &mut t.b,
            devs: &mut NoDevices,
        };
        assert_eq!(
            t.m.probe32(0x80100010, false, &mut p),
            None,
            "TLB ミスは不可（状態が変わるため）"
        );
        t.read(0x80100010, 4).unwrap();
        let mut p = BusPhys {
            bus: &mut t.b,
            devs: &mut NoDevices,
        };
        assert_eq!(t.m.probe32(0x80100010, false, &mut p), Some(0x1234));
        // setup で MMU を有効にした直後のフェッチ猶予中はフェッチとしては不可
        assert_eq!(t.m.fetch_grace, 2);
        assert_eq!(t.m.probe32(0x80100010, true, &mut p), None);
        t.m.fetch_grace = 0;
        assert_eq!(t.m.probe32(0x80100010, true, &mut p), Some(0x1234));
        assert_eq!(t.m.probe32(0x80100012, false, &mut p), None);
    }
}
