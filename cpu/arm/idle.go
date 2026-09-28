package arm

// アイドルループ検出（性能対策。ユーザー確認済み 2026-09）。
//
// WinCE の OAL はアイドル時に割り込み待ちのスピンをする（WM5 の実イメージでは
// 0x800AFDE4 の LDR r3,[r4] / CMP r3,#0 / BEQ の 3 命令。割り込みハンドラが
// RAM 上の変数を書くまで回り続ける。2026-09 の観察で、操作中でも実行命令の
// 約 85% がこのループだった）。
//
// このループは、割り込みが入るかロード先のメモリが書き換わるまで、何周しても
// CPU・メモリの状態を変えない（状態が周期 1 周で不動点になっている）。
// そこで実行ループ（machine）は、不動点であることを確かめたうえで、次に状態を
// 変え得る出来事（デバイスのイベント・外部入力）の直前まで命令数と仮想時間
// だけを進める。命令を 1 個ずつ実行した場合と全状態が完全に一致する。
//
// 不動点の確認方法（命令の意味をここで再実装しないため、実測で比べる）:
//  1. ループの形が「副作用のない 3 命令」であること（PollLoop が命令語を検査）:
//     LDR/LDRB 即値オフセット（ライトバックなし）→ TST/TEQ/CMP/CMN → 先頭への
//     B<cond>。どれもレジスタ・フラグ以外に書かない。
//  2. 命令フェッチとロードが、状態を変えずに済むアクセス（TLB ヒットの RAM）で
//     あること（cpu.Prober）。TLB の詰め替えやフェッチ猶予の消費、MMIO の
//     読み出しの副作用、-watch の表示が起きないことを保証する。
//  3. 連続する 2 周で、先頭に戻った時点の CPU 状態（PollState）が同一で、
//     その間がちょうど 3 命令（= 割り込み・例外が入っていない）であること
//     （machine が確認する）。さらにロード結果のレジスタがメモリの現在値と
//     一致すること（PollLoop が確認）。
//
// TODO: Thumb のループや 4 命令以上のループは未対応（現れたら追加する）。

// PollState はポーリングループの先頭での CPU 状態（2 周の一致判定用）。
// ループはモードを変えないので、見えているレジスタと CPSR で十分。
type PollState struct {
	regs [16]uint32
	cpsr PSR
}

// PollLoopLen はポーリングループの命令数。
const PollLoopLen = 3

// TakeSpinHint は直前の Step が 3 命令ループ先頭への後方分岐だったかを返し、
// 印を消す。実行ループが毎命令呼ぶので軽く保つこと。
func (c *Core) TakeSpinHint() bool {
	h := c.spinHint
	c.spinHint = false
	return h
}

// PollLoop は現在の PC が副作用のないポーリングループの先頭で、次の 1 周が
// 現在の状態を変えないと見込めるなら、その時点の状態を返す。1 周分の実測
// 一致（上記 3）は呼び出し側の責務。
func (c *Core) PollLoop() (PollState, bool) {
	if c.prober == nil || c.cpsr.T() || c.interruptPending() {
		return PollState{}, false
	}
	pc := c.regs[15]
	var w [PollLoopLen]uint32
	for i := range w {
		v, ok := c.prober.Probe32(pc+uint32(4*i), true)
		if !ok {
			return PollState{}, false
		}
		w[i] = v
	}
	ldr, cmp, br := w[0], w[1], w[2]
	// LDR/LDRB: cond=AL, 01 I=0 P=1 U=x B=x W=0 L=1
	if ldr&0xFF300000 != 0xE5100000 {
		return PollState{}, false
	}
	rn, rd := (ldr>>16)&0xF, (ldr>>12)&0xF
	if rn == 15 || rd == 15 || rn == rd {
		return PollState{}, false
	}
	// TST/TEQ/CMP/CMN（S=1）: cond=AL, 00 I opcode=10xx S=1。レジスタ形式は
	// シフト量が即値のもの（bit4=0）だけ（bit4=1 は乗算等と同居する空間）。
	if cmp&0xFD900000 != 0xE1100000 {
		return PollState{}, false
	}
	if cmp&(1<<25) == 0 && cmp&0x10 != 0 {
		return PollState{}, false
	}
	// B<cond>（L=0）で先頭へ: オフセット -16 バイト（PC+8 基準）。
	if br&0x0FFFFFFF != 0x0AFFFFFC {
		return PollState{}, false
	}
	// ロード先が状態を変えずに読めること、ロード結果がまだ rd にあること。
	off := ldr & 0xFFF
	addr := c.regs[rn] + off
	if ldr&(1<<23) == 0 {
		addr = c.regs[rn] - off
	}
	v, ok := c.prober.Probe32(addr&^3, false)
	if !ok {
		return PollState{}, false
	}
	if ldr&(1<<22) != 0 {
		v = (v >> (8 * (addr & 3))) & 0xFF
	} else {
		v = ror(v, 8*(addr&3))
	}
	if c.regs[rd] != v {
		return PollState{}, false
	}
	return PollState{regs: c.regs, cpsr: c.cpsr}, true
}

// interruptPending は次の Step が割り込みを受け付けるか（Step の先頭と同じ判定）。
func (c *Core) interruptPending() bool {
	return (c.fiq && c.cpsr&FlagF == 0) || (c.irq && c.cpsr&FlagI == 0)
}

// HistEntry は命令履歴の 1 件（Step を始めた時点の PC と状態）。
type HistEntry struct {
	PC    uint32
	Thumb bool
}

// SetHistory は直前 n 命令の履歴記録を有効にする（0 で無効）。停止原因の
// 調査用。命令語は表示時に読む（記録を軽くするため）。
// 内部のリングは 2 のべき乗に切り上げ、毎命令の記録を「PC（bit0 = Thumb）を
// 書いて添字を 1 増やす」だけにしている（剰余・分岐を避ける）。
func (c *Core) SetHistory(n int) {
	c.hist, c.histPos, c.histN = nil, 0, 0
	if n > 0 {
		size := 1
		for size < n {
			size <<= 1
		}
		c.hist = make([]uint32, size)
		c.histN = n
	}
}

func (c *Core) recordHistory() {
	c.hist[c.histPos&uint64(len(c.hist)-1)] = c.regs[15] | uint32(c.cpsr>>5)&1 // T = bit5
	c.histPos++
}

// SkipPollLoop は、実行ループがポーリングループを n 命令（周期の倍数）飛ばした
// ことを伝える。CPU の状態はループの不動点なので変えず、命令履歴にだけ
// 飛ばした区間の末尾（ループの PC 列）を補う（1 命令ずつ実行した場合と
// 同じ履歴を表示するため）。
func (c *Core) SkipPollLoop(n uint64) {
	if c.hist == nil {
		return
	}
	k := min(n, uint64(c.histN))
	head := c.regs[15]
	for i := n - k; i < n; i++ {
		c.regs[15] = head + 4*uint32(i%PollLoopLen)
		c.recordHistory()
	}
	c.regs[15] = head
}

// History は記録した履歴（最大 SetHistory の n 件）を古い順に返す。
func (c *Core) History() []HistEntry {
	n := min(uint64(c.histN), c.histPos)
	out := make([]HistEntry, 0, n)
	for i := c.histPos - n; i < c.histPos; i++ {
		v := c.hist[i&uint64(len(c.hist)-1)]
		out = append(out, HistEntry{PC: v &^ 1, Thumb: v&1 != 0})
	}
	return out
}
