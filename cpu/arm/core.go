// Package arm は ARMv4T（将来 v5TE 拡張予定）のインタプリタ実装。
// 仕様の根拠は ARM Architecture Reference Manual (DDI 0100)。
//
// 命令の「デコード」（decode.go）と「実行」（exec_*.go）を分離してある。
// これは将来、物理アドレス→デコード済み命令のキャッシュや、ブロック単位
// 実行を入れられるようにするため（iOS では JIT 不可なのでインタプリタが前提）。
package arm

import (
	"fmt"

	"github.com/mikuta0407/cerulean/cpu"
)

// バンクレジスタのインデックス。
// FIQ は r8-r14、IRQ/SVC/ABT/UND は r13-r14 が独立バンク。
// USR と SYS は全レジスタを共有する。
const (
	bankUsr = iota // usr/sys
	bankFiq
	bankIrq
	bankSvc
	bankAbt
	bankUnd
	numBanks
)

func bankIndex(mode uint32) int {
	switch mode {
	case ModeUsr, ModeSys:
		return bankUsr
	case ModeFiq:
		return bankFiq
	case ModeIrq:
		return bankIrq
	case ModeSvc:
		return bankSvc
	case ModeAbt:
		return bankAbt
	case ModeUnd:
		return bankUnd
	}
	return -1 // 不正なモード値（MSR で書かれた場合など）
}

// Coprocessor は MCR/MRC 命令の接続先（CP15 = MMU/キャッシュ制御を想定）。
// core 実装から MMU 実装への依存を切るための interface。
type Coprocessor interface {
	Read(opc1, crn, crm, opc2 uint8) (uint32, error)
	Write(opc1, crn, crm, opc2 uint8, v uint32) error
}

// Core は ARM CPU の状態。cpu.CPU を実装する。
type Core struct {
	mem  cpu.Memory
	cp15 Coprocessor // nil なら MCR/MRC p15 は未実装エラー

	// regs は現在のモードから見えるレジスタ。regs[15] = PC。
	// モード切替時に bankR13/bankR14（FIQ は bankR8Fiq も）と入れ替える。
	regs [16]uint32

	cpsr PSR
	spsr [numBanks]PSR // bankUsr は未使用（usr/sys に SPSR はない）

	// 退避領域。「現在モードでない側」の値を保持する。
	bankR8Usr [5]uint32 // r8-r12（FIQ 以外のモード用）
	bankR8Fiq [5]uint32 // r8-r12（FIQ 用）
	bankR13   [numBanks]uint32
	bankR14   [numBanks]uint32

	irq, fiq bool // 割り込み線のレベル（マイルストーン1では参照のみ）
}

var _ cpu.CPU = (*Core)(nil)

// New は mem に接続された ARM コアを作る。cp15 は nil でもよい
// （その場合 MCR/MRC p15 で未実装エラーになる）。
func New(mem cpu.Memory, cp15 Coprocessor) *Core {
	return &Core{mem: mem, cp15: cp15}
}

// Reset は電源投入相当。SVC モード・IRQ/FIQ 禁止・ARM state で pc から開始する。
// TODO: 本来のリセットはベクタ 0x00000000 へ飛ぶが、Device Emulator 同様に
// ローダーが決めたエントリポイントから直接開始する。実イメージで問題が出たら見直す。
func (c *Core) Reset(pc uint32) {
	*c = Core{mem: c.mem, cp15: c.cp15}
	c.cpsr = PSR(ModeSvc) | FlagI | FlagF
	c.regs[15] = pc
}

func (c *Core) PC() uint32 { return c.regs[15] }

func (c *Core) Reg(n int) uint32 { return c.regs[n&15] }

// SetReg はテスト・デバッガ用（現在モードのレジスタに書く）。
func (c *Core) SetReg(n int, v uint32) { c.regs[n&15] = v }

func (c *Core) CPSR() PSR     { return c.cpsr }
func (c *Core) SetCPSR(p PSR) { c.setCPSR(p) }
func (c *Core) SPSR() PSR     { return c.spsr[c.curBank()] }
func (c *Core) SetIRQ(a bool) { c.irq = a }
func (c *Core) SetFIQ(a bool) { c.fiq = a }

func (c *Core) curBank() int {
	if b := bankIndex(c.cpsr.Mode()); b >= 0 {
		return b
	}
	return bankUsr
}

// UndefinedError は未実装またはアーキテクチャ上未定義の命令。
// マイルストーン1では例外ベクタに飛ばさず、PC と命令語を持ってエミュレーションを止める。
type UndefinedError struct {
	PC     uint32
	Word   uint32
	Reason string
}

func (e *UndefinedError) Error() string {
	return fmt.Sprintf("unimplemented/undefined instruction at PC=%08X: word=%08X (%s)", e.PC, e.Word, e.Reason)
}

// Step は 1 命令実行する。エラー時は PC を命令の位置に戻して返す
// （レジスタ・メモリの部分的な副作用までは巻き戻さない）。
func (c *Core) Step() error {
	if c.cpsr.T() {
		// TODO(マイルストーン2): Thumb デコーダ。BX で Thumb に入るコードが
		// 来たらここで止まる。
		return &UndefinedError{PC: c.regs[15], Reason: "Thumb state not implemented yet"}
	}

	pc := c.regs[15]
	word, err := c.mem.Read32(pc)
	if err != nil {
		return fmt.Errorf("instruction fetch at PC=%08X: %w", pc, err)
	}

	in := Decode(word)

	// 実行中は regs[15] = PC+4 にしておく。オペランドとして r15 を読むときは
	// readReg が さらに +4 して「PC+8」（パイプラインの見え方）を返す。
	c.regs[15] = pc + 4

	if !condPassed(c.cpsr, word>>28) {
		return nil // 条件不成立: 何もせず次の命令へ
	}
	if err := in.exec(c, word); err != nil {
		if ue, ok := err.(*UndefinedError); ok {
			ue.PC = pc
			ue.Word = word
		}
		c.regs[15] = pc
		return err
	}
	return nil
}

// readReg はオペランドとしてのレジスタ読み出し。r15 は PC+8 に見える。
func (c *Core) readReg(n uint32) uint32 {
	if n == 15 {
		return c.regs[15] + 4 // Step で +4 済みなので、これで PC+8
	}
	return c.regs[n]
}

// writeReg は結果の書き込み。r15 への書き込みは分岐（ARMv4T: bit1:0 は
// 強制的に無視される。r15 書き込みで Thumb へは切り替わらない）。
func (c *Core) writeReg(n uint32, v uint32) {
	if n == 15 {
		c.regs[15] = v &^ 3
		return
	}
	c.regs[n] = v
}

// setCPSR はモード変更を含む CPSR 書き込み。バンク切替を行う。
func (c *Core) setCPSR(p PSR) {
	oldMode := c.cpsr.Mode()
	newMode := p.Mode()
	if oldMode != newMode {
		c.swapBanks(oldMode, newMode)
	}
	c.cpsr = p
}

// swapBanks は regs の r8-r14 を旧モードの退避領域に保存し、新モードの値をロードする。
func (c *Core) swapBanks(oldMode, newMode uint32) {
	oldBank := bankIndex(oldMode)
	newBank := bankIndex(newMode)
	if oldBank < 0 || newBank < 0 || oldBank == newBank {
		return
	}
	// r8-r12: FIQ とそれ以外の 2 バンクのみ。
	if (oldBank == bankFiq) != (newBank == bankFiq) {
		if oldBank == bankFiq {
			copy(c.bankR8Fiq[:], c.regs[8:13])
			copy(c.regs[8:13], c.bankR8Usr[:])
		} else {
			copy(c.bankR8Usr[:], c.regs[8:13])
			copy(c.regs[8:13], c.bankR8Fiq[:])
		}
	}
	// r13/r14: モードごと。
	c.bankR13[oldBank] = c.regs[13]
	c.bankR14[oldBank] = c.regs[14]
	c.regs[13] = c.bankR13[newBank]
	c.regs[14] = c.bankR14[newBank]
}

// 例外ベクタアドレス。
// TODO: CP15 の V ビット（high vectors 0xFFFF0000）は未対応。WinCE は
// high vectors を使うはずなので、MMU 実装時に対応する。
const (
	VecReset = 0x00
	VecUndef = 0x04
	VecSWI   = 0x08
	VecPabt  = 0x0C
	VecDabt  = 0x10
	VecIRQ   = 0x18
	VecFIQ   = 0x1C
)

// enterException は例外エントリの共通処理（ARM ARM A2.6）。
// retAddr は例外からの復帰用に LR_<mode> に入れる値。
func (c *Core) enterException(vector uint32, newMode uint32, retAddr uint32) {
	oldCPSR := c.cpsr

	p := (c.cpsr &^ 0x1F) | PSR(newMode)
	p &^= FlagT // 例外は常に ARM state で受ける
	p |= FlagI  // IRQ 禁止
	if newMode == ModeFiq {
		p |= FlagF
	}
	c.setCPSR(p)

	c.spsr[c.curBank()] = oldCPSR
	c.regs[14] = retAddr
	c.regs[15] = vector
}
