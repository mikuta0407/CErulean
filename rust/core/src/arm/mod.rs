//! ARMv4T（将来 v5TE 拡張予定）のインタプリタ。Go の cpu/arm パッケージ。
//! 仕様の根拠は ARM Architecture Reference Manual (DDI 0100)。
//!
//! 命令の「デコード」（[`decode`]）と「実行」（exec_arm.rs・exec_thumb.rs）を
//! 分離してある。これは物理アドレス→デコード済み命令のキャッシュや、ブロック
//! 単位実行・将来の IR/JIT を入れられるようにするため（iOS では JIT 不可なので
//! インタプリタが前提）。
//!
//! 所有権の設計（計画書 §3.3 の案 A）: [`Cpu`] はレジスタ・バンク・PSR だけの
//! データで、メモリ（MMU）・CP15・割り込み線・実行の上限は [`System`] として
//! 呼び出し側（machine の sys）から借りる。Go の Core が持っていた mem・cp15・
//! irq/fiq・runN/runBudget に当たるものは System の側にある。
//!
//! TODO(段階1 の高速化): Go のデコードキャッシュ（codecache.go）・頻出命令の
//! 特化（special.go）・アイドルループ検出（idle.go）・LDM/STM の RAMRun は、
//! 素直な実装で Go との一致を確かめてから 1 つずつ移す。

mod code;
mod exec_arm;
mod exec_thumb;
mod idle;
mod special;
#[cfg(test)]
mod tests;

use std::fmt;

use crate::bus::{BusError, RamOff};
use crate::cpu::{Abort, MemError};

pub use code::{CodeCache, CodeMemory, Instr};
pub use exec_arm::{ExecFn, decode_instr};
pub use idle::{POLL_LOOP_LEN, PollState};

// ---- PSR（CPSR/SPSR）----
//
//   31 30 29 28      7 6 5   4:0
//   N  Z  C  V  ...  I F T   mode

pub const FLAG_N: u32 = 1 << 31; // Negative
pub const FLAG_Z: u32 = 1 << 30; // Zero
pub const FLAG_C: u32 = 1 << 29; // Carry / not-borrow
pub const FLAG_V: u32 = 1 << 28; // oVerflow
pub const FLAG_I: u32 = 1 << 7; // IRQ 禁止
pub const FLAG_F: u32 = 1 << 6; // FIQ 禁止
pub const FLAG_T: u32 = 1 << 5; // Thumb state

// プロセッサモード（PSR bits 4:0）。
pub const MODE_USR: u32 = 0x10;
pub const MODE_FIQ: u32 = 0x11;
pub const MODE_IRQ: u32 = 0x12;
pub const MODE_SVC: u32 = 0x13;
pub const MODE_ABT: u32 = 0x17;
pub const MODE_UND: u32 = 0x1B;
pub const MODE_SYS: u32 = 0x1F;

/// PSR のフラグを立てる/落とす。
#[inline(always)]
fn set_flag(p: u32, flag: u32, b: bool) -> u32 {
    if b { p | flag } else { p & !flag }
}

/// 結果 v から N/Z を更新する（論理演算用: C は呼び出し側でシフタキャリーを入れる）。
#[inline(always)]
fn set_nz(p: u32, v: u32) -> u32 {
    set_flag(set_flag(p, FLAG_N, v & 0x80000000 != 0), FLAG_Z, v == 0)
}

/// 命令の条件フィールド（bits 31:28）が成立するか。
///
/// 0xF は ARMv4 では UNPREDICTABLE、v5 では拡張命令空間。Go 版はここで
/// 「不成立」を返すので、ARM 状態の cond=1111 の命令は（デコードでは未実装
/// 扱いでも）実行ループの条件判定で NOP として飛ばされる。段階1 は Go の
/// 動作に合わせる（TODO(v5TE): PXA27x 対応時に BLX(1) 等と合わせて見直す）。
pub(crate) fn cond_passed(p: u32, cond: u32) -> bool {
    let (n, z, c, v) = (
        p & FLAG_N != 0,
        p & FLAG_Z != 0,
        p & FLAG_C != 0,
        p & FLAG_V != 0,
    );
    match cond {
        0x0 => z,            // EQ
        0x1 => !z,           // NE
        0x2 => c,            // CS/HS
        0x3 => !c,           // CC/LO
        0x4 => n,            // MI
        0x5 => !n,           // PL
        0x6 => v,            // VS
        0x7 => !v,           // VC
        0x8 => c && !z,      // HI
        0x9 => !c || z,      // LS
        0xA => n == v,       // GE
        0xB => n != v,       // LT
        0xC => !z && n == v, // GT
        0xD => z || n != v,  // LE
        0xE => true,         // AL
        _ => false,          // 0xF: 上記
    }
}

/// cond_passed の表（COND_TABLE[cond<<4 | NZCV]。実行ループの高速化用）。
static COND_TABLE: [bool; 256] = {
    let mut t = [false; 256];
    let mut i = 0;
    while i < 256 {
        let (cond, f) = ((i >> 4) as u32, (i & 15) as u32);
        let (n, z, c, v) = (f & 8 != 0, f & 4 != 0, f & 2 != 0, f & 1 != 0);
        t[i] = match cond {
            0x0 => z,
            0x1 => !z,
            0x2 => c,
            0x3 => !c,
            0x4 => n,
            0x5 => !n,
            0x6 => v,
            0x7 => !v,
            0x8 => c && !z,
            0x9 => !c || z,
            0xA => n == v,
            0xB => n != v,
            0xC => !z && n == v,
            0xD => z || n != v,
            0xE => true,
            _ => false,
        };
        i += 1;
    }
    t
};

// ---- バンク ----
// FIQ は r8-r14、IRQ/SVC/ABT/UND は r13-r14 が独立バンク。
// USR と SYS は全レジスタを共有する。
pub(crate) const BANK_USR: usize = 0; // usr/sys
pub(crate) const BANK_FIQ: usize = 1;
pub(crate) const BANK_IRQ: usize = 2;
pub(crate) const BANK_SVC: usize = 3;
pub(crate) const BANK_ABT: usize = 4;
pub(crate) const BANK_UND: usize = 5;
pub(crate) const NUM_BANKS: usize = 6;

/// モード値のバンク（不正なモード値なら None。MSR で書かれた場合など）。
fn bank_index(mode: u32) -> Option<usize> {
    match mode {
        MODE_USR | MODE_SYS => Some(BANK_USR),
        MODE_FIQ => Some(BANK_FIQ),
        MODE_IRQ => Some(BANK_IRQ),
        MODE_SVC => Some(BANK_SVC),
        MODE_ABT => Some(BANK_ABT),
        MODE_UND => Some(BANK_UND),
        _ => None,
    }
}

// 例外ベクタオフセット。ベースは CP15 の V ビットで 0 または 0xFFFF0000
// （enter_exception が System::vector_base で解決する）。
pub const VEC_RESET: u32 = 0x00;
pub const VEC_UNDEF: u32 = 0x04;
pub const VEC_SWI: u32 = 0x08;
pub const VEC_PABT: u32 = 0x0C;
pub const VEC_DABT: u32 = 0x10;
pub const VEC_IRQ: u32 = 0x18;
pub const VEC_FIQ: u32 = 0x1C;

/// Go の Core.Run の runN/runBudget。machine（時間の同期で上限を下げる）と
/// CPU が両方触るので、System の側に置く。
#[derive(Clone, Copy, Debug, Default)]
pub struct RunCtl {
    /// 今の run で実行を終えた命令数（実行中の命令は含まない）
    pub n: u64,
    /// ここまでで止まる上限（実行中に machine が下げる）
    pub budget: u64,
}

impl RunCtl {
    /// 実行中の run を、通算 n 命令（self.n の値）で止めるよう上限を下げる
    /// （Go の LimitRun）。デバイスのイベント期限が早まったときに使う。
    #[inline]
    pub fn limit(&mut self, n: u64) {
        if n < self.budget {
            self.budget = n;
        }
    }
}

/// CPU から見たシステム（メモリ・CP15・割り込み線・実行の上限）。machine の
/// sys が実装する。Go の cpu.Memory・arm.Coprocessor・SetIRQ/SetFIQ の組。
pub trait System {
    /// データの読み出し（size は 1/2/4。アドレスは CPU が発行したまま）。
    fn read(&mut self, va: u32, size: u32) -> Result<u32, MemError>;
    /// データの書き込み（size は 1/2/4。v の下位 size バイト）。
    fn write(&mut self, va: u32, size: u32, v: u32) -> Result<(), MemError>;
    /// ARM 命令のフェッチ。MMU 有効化直後のパイプライン近似（フェッチ猶予）が
    /// あるので、データの読み出しと区別する（Go の InstructionFetcher）。
    fn fetch32(&mut self, va: u32) -> Result<u32, MemError>;
    /// MRC p15。
    fn cp15_read(&mut self, opc1: u8, crn: u8, crm: u8, opc2: u8) -> u32;
    /// MCR p15。
    fn cp15_write(&mut self, opc1: u8, crn: u8, crm: u8, opc2: u8, v: u32);
    /// 例外ベクタのベースアドレス（CP15 制御レジスタ V ビット: 0 または 0xFFFF0000）。
    fn vector_base(&self) -> u32;
    /// CPU の特権状態の変化を伝える。MMU のアクセス権限チェック（AP ビット）が
    /// 特権/ユーザーで異なるため。アクセスごとにモードを渡すより、変化時に
    /// 通知する方が軽い。
    fn set_privileged(&mut self, privileged: bool);
    /// データアボートの FSR/FAR を更新する。
    fn record_data_abort(&mut self, a: &Abort);
    /// 割り込み線のレベル（INTC の出力）。命令境界で読む。
    fn irq(&self) -> bool;
    fn fiq(&self) -> bool;
    /// 実行の上限（run の間、CPU が n を進め、machine が budget を下げる）。
    fn run_ctl(&mut self) -> &mut RunCtl;

    /// ARM 命令をフェッチしてデコードする。既定はキャッシュなし（fetch32 と
    /// デコード）。デコードキャッシュを持つシステムは上書きする（code.rs）。
    /// フェッチのアボート・バスエラーは fetch32 と同じく Err で返す。
    #[inline(always)]
    fn fetch_arm(&mut self, pc: u32) -> Result<Instr<Self>, MemError>
    where
        Self: Sized,
    {
        Ok(decode_instr::<Self>(self.fetch32(pc)?))
    }

    /// 状態を変えずに読める場合だけ 32 ビットを読む（アイドルループ検出用。
    /// Go の cpu.Prober）。Some を返すのは、同じアクセスを実際に行っても
    /// メモリ側の状態（ソフト TLB・フェッチ猶予・監視の記録など）が一切変わらない
    /// 場合だけ。既定は常に None（検出しない）。
    fn probe32(&mut self, _va: u32, _fetch: bool) -> Option<u32> {
        None
    }

    /// va から nbytes バイト（同じ 4KB ページ内）を直接読み書きしてよい RAM の範囲の
    /// 先頭位置を返す（LDM/STM の高速化用。Go の RAMRun）。TLB ヒット等の条件を
    /// 満たすときだけ Some で、状態を変えない。既定は常に None（1 ワードずつ）。
    fn ram_run(&mut self, _va: u32, _nbytes: u32, _write: bool) -> Option<RamOff> {
        None
    }

    /// ram_run が返した範囲のワードを読む。
    fn ram_word(&self, _off: RamOff) -> u32 {
        unreachable!("ram_word without ram_run")
    }

    /// ram_run（write=true）が返した範囲にワードを書く。
    fn set_ram_word(&mut self, _off: RamOff, _v: u32) {
        unreachable!("set_ram_word without ram_run")
    }
}

/// 未実装またはアーキテクチャ上未定義の命令。
/// `arch=true` は「実機（ARM920T 構成）でも未定義命令例外になる」ことが
/// 確かな命令で、ゲストに例外として配送する（WinCE は FPU 検出などで意図的に
/// 未定義命令を実行する）。`arch=false` はエミュレータの実装漏れの可能性が
/// あるため、例外にせず停止して気づけるようにする。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UndefinedError {
    pub pc: u32,
    pub word: u32,
    pub reason: &'static str,
    pub arch: bool,
}

impl fmt::Display for UndefinedError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "unimplemented/undefined instruction at PC={:08X}: word={:08X} ({})",
            self.pc, self.word, self.reason
        )
    }
}

/// エミュレーションを止めるエラー（ゲストに配送しないもの）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopError {
    Undefined(UndefinedError),
    /// 未マップの物理アドレス（命令フェッチ・データアクセス・ページテーブル）。
    Bus(BusError),
}

impl fmt::Display for StopError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StopError::Undefined(u) => u.fmt(f),
            StopError::Bus(b) => b.fmt(f),
        }
    }
}

impl std::error::Error for StopError {}

/// 命令の実行中のエラー（exec 関数が返す）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Exec {
    Mem(MemError),
    Undef { reason: &'static str, arch: bool },
}

impl From<MemError> for Exec {
    #[inline(always)]
    fn from(e: MemError) -> Self {
        Exec::Mem(e)
    }
}

pub type ExecResult = Result<(), Exec>;

pub(crate) fn unimpl(reason: &'static str) -> Exec {
    Exec::Undef {
        reason,
        arch: false,
    }
}

/// ARM CPU の状態。
///
/// スナップショットに保存するのはすべてのフィールド（hist を除く）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cpu {
    /// 現在のモードから見えるレジスタ。regs[15] = PC。
    /// モード切替時に bank_r13/bank_r14（FIQ は bank_r8_fiq も）と入れ替える。
    pub(crate) regs: [u32; 16],
    pub(crate) cpsr: u32,
    /// BANK_USR は未使用（usr/sys に SPSR はない）
    pub(crate) spsr: [u32; NUM_BANKS],
    // 退避領域。「現在モードでない側」の値を保持する。
    /// r8-r12（FIQ 以外のモード用）
    pub(crate) bank_r8_usr: [u32; 5],
    /// r8-r12（FIQ 用）
    pub(crate) bank_r8_fiq: [u32; 5],
    pub(crate) bank_r13: [u32; NUM_BANKS],
    pub(crate) bank_r14: [u32; NUM_BANKS],
    /// 直前に実行を始めた命令の記録（デバッグ用。派生情報で保存しない）。
    hist: History,
    /// 「直前の命令が 3 命令ループ先頭への後方分岐だった」印（idle.rs）。
    /// 実行ループ（machine）が run のたびに見て消費する一時的な値（保存しない）。
    pub(crate) spin_hint: bool,
}

impl Default for Cpu {
    fn default() -> Self {
        Self::new()
    }
}

impl Cpu {
    pub fn new() -> Cpu {
        Cpu {
            regs: [0; 16],
            cpsr: MODE_SVC | FLAG_I | FLAG_F,
            spsr: [0; NUM_BANKS],
            bank_r8_usr: [0; 5],
            bank_r8_fiq: [0; 5],
            bank_r13: [0; NUM_BANKS],
            bank_r14: [0; NUM_BANKS],
            hist: History::default(),
            spin_hint: false,
        }
    }

    /// 電源投入相当。SVC モード・IRQ/FIQ 禁止・ARM state で pc から開始する。
    /// 命令履歴の設定は保つ（中身は空にする）。
    /// TODO: 本来のリセットはベクタ 0x00000000 へ飛ぶが、Device Emulator 同様に
    /// ローダーが決めたエントリポイントから直接開始する。実イメージで問題が出たら見直す。
    pub fn reset(&mut self, pc: u32, sys: &mut impl System) {
        let n = self.hist.n;
        *self = Cpu::new();
        self.hist.set(n);
        self.regs[15] = pc;
        sys.set_privileged(true);
    }

    /// 現在のプログラムカウンタ（次に実行する命令のアドレス）。
    pub fn pc(&self) -> u32 {
        self.regs[15]
    }

    /// 現在のモードから見える汎用レジスタ r0..r15。
    pub fn reg(&self, n: usize) -> u32 {
        self.regs[n & 15]
    }

    /// テスト・デバッガ用（現在モードのレジスタに書く）。
    pub fn set_reg(&mut self, n: usize, v: u32) {
        self.regs[n & 15] = v;
    }

    pub fn cpsr(&self) -> u32 {
        self.cpsr
    }

    /// Thumb 状態か。
    pub fn thumb(&self) -> bool {
        self.cpsr & FLAG_T != 0
    }

    /// モード変更を含む CPSR 書き込み（テスト・デバッガ用）。
    pub fn set_cpsr(&mut self, p: u32, sys: &mut impl System) {
        self.write_cpsr(p, sys);
    }

    pub(crate) fn cur_bank(&self) -> usize {
        bank_index(self.cpsr & 0x1F).unwrap_or(BANK_USR)
    }

    /// オペランドとしてのレジスタ読み出し。r15 は PC+8 に見える。
    #[inline(always)]
    pub(crate) fn read_reg(&self, n: u32) -> u32 {
        if n == 15 {
            self.regs[15].wrapping_add(4) // 実行中は +4 済みなので、これで PC+8
        } else {
            self.regs[n as usize]
        }
    }

    /// 結果の書き込み。r15 への書き込みは分岐（ARMv4T: bit1:0 は強制的に
    /// 無視される。r15 書き込みで Thumb へは切り替わらない）。
    #[inline(always)]
    pub(crate) fn write_reg(&mut self, n: u32, v: u32) {
        if n == 15 {
            self.regs[15] = v & !3;
        } else {
            self.regs[n as usize] = v;
        }
    }

    /// モード変更を含む CPSR 書き込み。バンク切替を行う。
    pub(crate) fn write_cpsr(&mut self, p: u32, sys: &mut impl System) {
        let old_mode = self.cpsr & 0x1F;
        let new_mode = p & 0x1F;
        if old_mode != new_mode {
            self.swap_banks(old_mode, new_mode);
            // MMU の権限チェック用に特権状態の変化を伝える（usr だけが非特権）。
            sys.set_privileged(new_mode != MODE_USR);
        }
        self.cpsr = p;
    }

    /// regs の r8-r14 を旧モードの退避領域に保存し、新モードの値をロードする。
    fn swap_banks(&mut self, old_mode: u32, new_mode: u32) {
        let (Some(ob), Some(nb)) = (bank_index(old_mode), bank_index(new_mode)) else {
            return;
        };
        if ob == nb {
            return;
        }
        // r8-r12: FIQ とそれ以外の 2 バンクのみ。
        if (ob == BANK_FIQ) != (nb == BANK_FIQ) {
            if ob == BANK_FIQ {
                self.bank_r8_fiq.copy_from_slice(&self.regs[8..13]);
                self.regs[8..13].copy_from_slice(&self.bank_r8_usr);
            } else {
                self.bank_r8_usr.copy_from_slice(&self.regs[8..13]);
                self.regs[8..13].copy_from_slice(&self.bank_r8_fiq);
            }
        }
        // r13/r14: モードごと。
        self.bank_r13[ob] = self.regs[13];
        self.bank_r14[ob] = self.regs[14];
        self.regs[13] = self.bank_r13[nb];
        self.regs[14] = self.bank_r14[nb];
    }

    /// 例外エントリの共通処理（ARM ARM A2.6）。
    /// ret_addr は例外からの復帰用に LR_<mode> に入れる値。
    pub(crate) fn enter_exception(
        &mut self,
        vector: u32,
        new_mode: u32,
        ret_addr: u32,
        sys: &mut impl System,
    ) {
        let old = self.cpsr;
        let mut p = (self.cpsr & !0x1F) | new_mode;
        p &= !FLAG_T; // 例外は常に ARM state で受ける
        p |= FLAG_I; // IRQ 禁止
        if new_mode == MODE_FIQ {
            p |= FLAG_F;
        }
        self.write_cpsr(p, sys);
        let b = self.cur_bank();
        self.spsr[b] = old;
        self.regs[14] = ret_addr;
        self.regs[15] = sys.vector_base() | vector;
    }

    /// 命令実行中のエラーのうち ARM 例外として配送できるものを処理する。
    /// 配送したら Ok、できないもの（エミュレータ未実装・バスエラー）は Err。
    /// instr_len は未定義例外の LR 計算用（ARM=4, Thumb=2）。
    fn deliver_exec_error(
        &mut self,
        e: Exec,
        pc: u32,
        word: u32,
        instr_len: u32,
        sys: &mut impl System,
    ) -> Result<(), StopError> {
        match e {
            Exec::Mem(MemError::Abort(a)) => {
                // データアボート。FSR（ドメイン|ステータス）と FAR を更新して配送。
                // LR = PC+8（ARM/Thumb 共通。ハンドラは SUBS pc, lr, #8 で再実行できる）。
                sys.record_data_abort(&a);
                self.enter_exception(VEC_DABT, MODE_ABT, pc.wrapping_add(8), sys);
                Ok(())
            }
            Exec::Undef { arch: true, .. } => {
                // 実機でも未定義例外になる命令: ゲストに配送する。
                // LR = 未定義命令の次（ARM ARM A2.6.4）。
                self.enter_exception(VEC_UNDEF, MODE_UND, pc.wrapping_add(instr_len), sys);
                Ok(())
            }
            Exec::Undef { reason, arch } => Err(StopError::Undefined(UndefinedError {
                pc,
                word,
                reason,
                arch,
            })),
            Exec::Mem(MemError::Bus(b)) => Err(StopError::Bus(b)),
        }
    }

    /// 次の命令が割り込みを受け付けるか（run の先頭と同じ判定）。
    pub fn interrupt_pending(&self, sys: &impl System) -> bool {
        (sys.fiq() && self.cpsr & FLAG_F == 0) || (sys.irq() && self.cpsr & FLAG_I == 0)
    }

    /// 1 命令実行する（run(1) と同じ）。
    pub fn step(&mut self, sys: &mut impl System) -> Result<(), StopError> {
        self.run(sys, 1).map(|_| ())
    }

    /// 最大 budget 命令を実行し、実行した命令数を返す（ブロック実行。Go の Core.Run）。
    /// 次の場合は途中で戻る:
    ///   - エラー（エラーを起こした命令も 1 命令と数える）。CPU はその命令の
    ///     位置（PC）に戻る（レジスタ・メモリの部分的な副作用までは巻き戻さない）。
    ///   - 実行中に machine が上限（RunCtl::budget）を下げた
    ///
    /// MMU 起因のアボートは ARM の例外として配送して続ける。1 命令ずつ step した
    /// 場合と状態は同一。実行中に周辺機器が「今まで何命令実行したか」を知る
    /// 必要があるとき（仮想時間の同期）は sys の RunCtl::n を読む。
    pub fn run<S: System>(&mut self, sys: &mut S, budget: u64) -> Result<u64, StopError> {
        *sys.run_ctl() = RunCtl { n: 0, budget };
        loop {
            let rc = *sys.run_ctl();
            if rc.n >= rc.budget {
                return Ok(rc.n);
            }
            let r = self.step_one(sys);
            // n は命令を終えてから増やす（実行中の命令からデバイスが n を読むと、
            // 終えた命令数が返る）。
            sys.run_ctl().n += 1;
            r?;
        }
    }

    /// 命令を 1 個実行する（割り込みの受け付けを含む）。
    #[inline(always)]
    fn step_one<S: System>(&mut self, sys: &mut S) -> Result<(), StopError> {
        if self.hist.enabled() {
            self.hist.record(self.regs[15] | (self.cpsr >> 5) & 1);
        }
        // 割り込みは命令境界で受け付ける。FIQ が IRQ より優先（ARM ARM A2.6）。
        // 復帰先は「実行されなかった命令」なので LR = その PC+4
        // （ハンドラは SUBS pc, lr, #4 で戻る）。
        if sys.fiq() && self.cpsr & FLAG_F == 0 {
            self.enter_exception(VEC_FIQ, MODE_FIQ, self.regs[15].wrapping_add(4), sys);
            return Ok(());
        }
        if sys.irq() && self.cpsr & FLAG_I == 0 {
            self.enter_exception(VEC_IRQ, MODE_IRQ, self.regs[15].wrapping_add(4), sys);
            return Ok(());
        }
        if self.cpsr & FLAG_T != 0 {
            return self.step_thumb(sys);
        }

        let pc = self.regs[15];
        let ins = match sys.fetch_arm(pc) {
            Ok(i) => i,
            Err(MemError::Abort(_)) => {
                // プリフェッチアボート。FSR/FAR はデータアボート専用なので更新しない
                // （ARM ARM: FAR is only updated for data aborts）。LR = PC+4。
                self.enter_exception(VEC_PABT, MODE_ABT, pc.wrapping_add(4), sys);
                return Ok(());
            }
            Err(MemError::Bus(b)) => return Err(StopError::Bus(b)),
        };
        let word = ins.word;
        // 実行中は regs[15] = PC+4 にしておく。オペランドとして r15 を読むときは
        // read_reg がさらに +4 して「PC+8」（パイプラインの見え方）を返す。
        self.regs[15] = pc.wrapping_add(4);

        let cond = word >> 28;
        if cond != 0xE && !COND_TABLE[(cond << 4 | self.cpsr >> 28) as usize] {
            return Ok(()); // 条件不成立: 何もせず次の命令へ
        }
        if let Err(e) = (ins.exec)(self, sys, word, ins.imm)
            && let Err(stop) = self.deliver_exec_error(e, pc, word, 4, sys)
        {
            self.regs[15] = pc;
            return Err(stop);
        }
        Ok(())
    }

    // ---- 命令履歴（デバッグ用）----

    /// 直前 n 命令の履歴記録を有効にする（0 で無効）。停止原因の調査用。
    pub fn set_history(&mut self, n: usize) {
        self.hist.set(n);
    }

    /// 記録した履歴（最大 set_history の n 件）を古い順に返す（PC, Thumb か）。
    pub fn history(&self) -> Vec<(u32, bool)> {
        self.hist.entries()
    }

    // ---- 一致確認用 ----

    /// 全モードのレジスタを「そのモードから見た値」で並べたもの（Go の
    /// ArchRegs。testdata/golden/README の CPU 状態のダンプに使う）。
    pub fn arch_regs(&self) -> ArchRegs {
        let mut a = ArchRegs {
            r: self.regs,
            cpsr: self.cpsr,
            ..Default::default()
        };
        let cur = self.cur_bank();
        // r8〜r12 は FIQ とそれ以外の 2 組。
        if cur == BANK_FIQ {
            a.fiq[..5].copy_from_slice(&self.regs[8..13]);
            a.usr[..5].copy_from_slice(&self.bank_r8_usr);
        } else {
            a.usr[..5].copy_from_slice(&self.regs[8..13]);
            a.fiq[..5].copy_from_slice(&self.bank_r8_fiq);
        }
        // r13/r14 はモードごと。現在モードの分は regs から（退避領域の枠は古い）。
        let r1314 = |b: usize| {
            if b == cur {
                [self.regs[13], self.regs[14]]
            } else {
                [self.bank_r13[b], self.bank_r14[b]]
            }
        };
        let u = r1314(BANK_USR);
        (a.usr[5], a.usr[6]) = (u[0], u[1]);
        let f = r1314(BANK_FIQ);
        (a.fiq[5], a.fiq[6]) = (f[0], f[1]);
        a.irq = r1314(BANK_IRQ);
        a.svc = r1314(BANK_SVC);
        a.abt = r1314(BANK_ABT);
        a.und = r1314(BANK_UND);
        for (i, b) in [BANK_FIQ, BANK_IRQ, BANK_SVC, BANK_ABT, BANK_UND]
            .into_iter()
            .enumerate()
        {
            a.spsr[i] = self.spsr[b];
        }
        a
    }
}

/// 全モードのレジスタ（[`Cpu::arch_regs`]）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ArchRegs {
    /// 現在モードから見える r0〜r15
    pub r: [u32; 16],
    /// usr/sys の r8〜r14
    pub usr: [u32; 7],
    /// fiq の r8〜r14
    pub fiq: [u32; 7],
    /// irq・svc・abt・und の r13, r14
    pub irq: [u32; 2],
    pub svc: [u32; 2],
    pub abt: [u32; 2],
    pub und: [u32; 2],
    pub cpsr: u32,
    /// fiq, irq, svc, abt, und の順
    pub spsr: [u32; 5],
}

/// 命令履歴のリング（Go の hist）。容量は 2 のべき乗に切り上げ、毎命令の記録を
/// 「PC（bit0 = Thumb）を書いて添字を 1 増やす」だけにしている。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct History {
    buf: Vec<u32>,
    /// 次に書く位置（通算）
    pos: u64,
    /// 表示する件数
    pub(crate) n: usize,
}

impl History {
    fn set(&mut self, n: usize) {
        *self = History::default();
        if n > 0 {
            self.buf = vec![0; n.next_power_of_two()];
            self.n = n;
        }
    }

    #[inline(always)]
    pub(crate) fn enabled(&self) -> bool {
        !self.buf.is_empty()
    }

    #[inline(always)]
    pub(crate) fn record(&mut self, v: u32) {
        let mask = self.buf.len() as u64 - 1;
        self.buf[(self.pos & mask) as usize] = v;
        self.pos += 1;
    }

    fn entries(&self) -> Vec<(u32, bool)> {
        if !self.enabled() {
            return vec![];
        }
        let n = (self.n as u64).min(self.pos);
        let mask = self.buf.len() as u64 - 1;
        (self.pos - n..self.pos)
            .map(|i| {
                let v = self.buf[(i & mask) as usize];
                (v & !1, v & 1 != 0)
            })
            .collect()
    }
}

// ---- スナップショット ----

impl Cpu {
    pub const STATE_VERSION: u16 = 1;

    /// 保存する状態（命令履歴・spin_hint は派生情報なので保存しない）。
    pub fn save_state(&self, e: &mut crate::snapshot::Encoder) {
        let Cpu {
            regs,
            cpsr,
            spsr,
            bank_r8_usr,
            bank_r8_fiq,
            bank_r13,
            bank_r14,
            hist: _,
            spin_hint: _,
        } = self;
        e.u32s(regs);
        e.u32(*cpsr);
        e.u32s(spsr);
        e.u32s(bank_r8_usr);
        e.u32s(bank_r8_fiq);
        e.u32s(bank_r13);
        e.u32s(bank_r14);
    }

    /// 読み込む（命令履歴の設定は保ち、中身は空にする）。
    pub fn load_state(
        &mut self,
        d: &mut crate::snapshot::Decoder,
    ) -> Result<(), crate::snapshot::Error> {
        let Cpu {
            regs,
            cpsr,
            spsr,
            bank_r8_usr,
            bank_r8_fiq,
            bank_r13,
            bank_r14,
            hist,
            spin_hint,
        } = self;
        *regs = d.u32s()?;
        *cpsr = d.u32()?;
        *spsr = d.u32s()?;
        *bank_r8_usr = d.u32s()?;
        *bank_r8_fiq = d.u32s()?;
        *bank_r13 = d.u32s()?;
        *bank_r14 = d.u32s()?;
        let n = hist.n;
        hist.set(n);
        *spin_hint = false;
        Ok(())
    }
}
