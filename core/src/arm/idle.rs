//! アイドルループ検出（性能対策）。
//!
//! WinCE の OAL はアイドル時に割り込み待ちのスピンをする（WM5 の実イメージでは
//! 0x800AFDE4 の LDR r3,[r4] / CMP r3,#0 / BEQ の 3 命令。割り込みハンドラが
//! RAM 上の変数を書くまで回り続ける。2026-09 の観察で、操作中でも実行命令の
//! 約 85% がこのループだった）。
//!
//! このループは、割り込みが入るかロード先のメモリが書き換わるまで、何周しても
//! CPU・メモリの状態を変えない（状態が周期 1 周で不動点になっている）。
//! そこで実行ループ（machine）は、不動点であることを確かめたうえで、次に状態を
//! 変え得る出来事（デバイスのイベント・外部入力）の直前まで命令数と仮想時間
//! だけを進める。命令を 1 個ずつ実行した場合と全状態が完全に一致する。
//!
//! 不動点の確認方法（命令の意味をここで再実装しないため、実測で比べる）:
//!  1. ループの形が「副作用のない 3 命令」であること（poll_loop が命令語を検査）:
//!     LDR/LDRB 即値オフセット（ライトバックなし）→ TST/TEQ/CMP/CMN → 先頭への
//!     B<cond>。どれもレジスタ・フラグ以外に書かない。
//!  2. 命令フェッチとロードが、状態を変えずに済むアクセス（TLB ヒットの RAM）で
//!     あること（System::probe32）。TLB の詰め替えやフェッチ猶予の消費、MMIO の
//!     読み出しの副作用、監視の記録が起きないことを保証する。
//!  3. 連続する 2 周で、先頭に戻った時点の CPU 状態（PollState）が同一で、
//!     その間がちょうど 3 命令（= 割り込み・例外が入っていない）であること
//!     （machine が確認する）。さらにロード結果のレジスタがメモリの現在値と
//!     一致すること（poll_loop が確認）。
//!
//! TODO: Thumb のループや 4 命令以上のループは未対応（現れたら追加する）。

use super::exec_arm::ror;
use super::*;

/// ポーリングループの先頭での CPU 状態（2 周の一致判定用）。
/// ループはモードを変えないので、見えているレジスタと CPSR で十分。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PollState {
    regs: [u32; 16],
    cpsr: u32,
}

/// ポーリングループの命令数。
pub const POLL_LOOP_LEN: u64 = 3;

impl Cpu {
    /// 直前の run が 3 命令ループ先頭への後方分岐で終わったかを返し、印を消す。
    pub fn take_spin_hint(&mut self) -> bool {
        std::mem::take(&mut self.spin_hint)
    }

    /// 現在の PC が副作用のないポーリングループの先頭で、次の 1 周が現在の状態を
    /// 変えないと見込めるなら、その時点の状態を返す。1 周分の実測一致（上記 3）は
    /// 呼び出し側の責務。
    pub fn poll_loop(&self, sys: &mut impl System) -> Option<PollState> {
        if self.thumb() || self.interrupt_pending(sys) {
            return None;
        }
        let pc = self.regs[15];
        let mut w = [0u32; POLL_LOOP_LEN as usize];
        for (i, x) in w.iter_mut().enumerate() {
            *x = sys.probe32(pc.wrapping_add(4 * i as u32), true)?;
        }
        let [ldr, cmp, br] = w;
        // LDR/LDRB: cond=AL, 01 I=0 P=1 U=x B=x W=0 L=1
        if ldr & 0xFF300000 != 0xE5100000 {
            return None;
        }
        let (rn, rd) = ((ldr >> 16) & 0xF, (ldr >> 12) & 0xF);
        if rn == 15 || rd == 15 || rn == rd {
            return None;
        }
        // TST/TEQ/CMP/CMN（S=1）: cond=AL, 00 I opcode=10xx S=1。レジスタ形式は
        // シフト量が即値のもの（bit4=0）だけ（bit4=1 は乗算等と同居する空間）。
        if cmp & 0xFD900000 != 0xE1100000 || (cmp & (1 << 25) == 0 && cmp & 0x10 != 0) {
            return None;
        }
        // B<cond>（L=0）で先頭へ: オフセット -16 バイト（PC+8 基準）。
        if br & 0x0FFFFFFF != 0x0AFFFFFC {
            return None;
        }
        // ロード先が状態を変えずに読めること、ロード結果がまだ rd にあること。
        let off = ldr & 0xFFF;
        let base = self.regs[rn as usize];
        let addr = if ldr & (1 << 23) == 0 {
            base.wrapping_sub(off)
        } else {
            base.wrapping_add(off)
        };
        let v = sys.probe32(addr & !3, false)?;
        let v = if ldr & (1 << 22) != 0 {
            (v >> (8 * (addr & 3))) & 0xFF
        } else {
            ror(v, 8 * (addr & 3))
        };
        if self.regs[rd as usize] != v {
            return None;
        }
        Some(PollState {
            regs: self.regs,
            cpsr: self.cpsr,
        })
    }

    /// 実行ループがポーリングループを n 命令（周期の倍数）飛ばしたことを伝える。
    /// CPU の状態はループの不動点なので変えず、命令履歴にだけ飛ばした区間の末尾
    /// （ループの PC 列）を補う（1 命令ずつ実行した場合と同じ履歴を表示するため）。
    pub fn skip_poll_loop(&mut self, n: u64) {
        if !self.hist.enabled() {
            return;
        }
        let k = n.min(self.hist.n as u64);
        let head = self.regs[15];
        for i in n - k..n {
            self.hist
                .record(head.wrapping_add(4 * (i % POLL_LOOP_LEN) as u32));
        }
    }
}
