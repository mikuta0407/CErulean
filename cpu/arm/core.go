// Package arm は ARMv4T（将来 v5TE 拡張予定）のインタプリタ実装。
// 仕様の根拠は ARM Architecture Reference Manual (DDI 0100)。
//
// 命令の「デコード」（decode.go）と「実行」（exec_*.go）を分離してある。
// これは将来、物理アドレス→デコード済み命令のキャッシュや、ブロック単位
// 実行を入れられるようにするため（iOS では JIT 不可なのでインタプリタが前提）。
package arm

import (
	"errors"
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
	// VectorBase は例外ベクタのベースアドレス
	// （CP15 制御レジスタ V ビット: 0 または 0xFFFF0000）。
	VectorBase() uint32
	// SetPrivileged は CPU の特権状態の変化を伝える。MMU のアクセス権限
	// チェック（AP ビット）が特権/ユーザーで異なるため。cpu.Memory
	// interface にモード引数を足すより、変化時に通知する方が軽い。
	SetPrivileged(priv bool)
}

// Core は ARM CPU の状態。cpu.CPU を実装する。
type Core struct {
	mem  cpu.Memory
	cp15 Coprocessor // nil なら MCR/MRC p15 は未実装エラー

	// fetch32 は命令フェッチ用の読み出し。mem が cpu.InstructionFetcher を
	// 実装していればそれ（MMU 有効化直後のパイプライン近似のため）、
	// なければ mem.Read32。
	fetch32 func(addr uint32) (uint32, error)
	// prober は mem が cpu.Prober を実装していればそれ（アイドルループ検出用。
	// なければ検出しない）。
	prober cpu.Prober

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

	// spinHint は「直前の命令が 3 命令ループ先頭への後方分岐だった」印
	// （idle.go）。実行ループ（machine）が毎命令見て消費する一時的な値。
	spinHint bool

	// hist は直前に実行を始めた命令の記録（デバッグ用のリングバッファ。
	// SetHistory で有効化。nil なら記録しない）。
	hist    []uint32 // PC | Thumb(bit0)。長さは 2 のべき乗
	histPos uint64   // 次に書く位置（通算。添字は len-1 でマスク）
	histN   int      // 表示する件数
}

var _ cpu.CPU = (*Core)(nil)

// New は mem に接続された ARM コアを作る。cp15 は nil でもよい
// （その場合 MCR/MRC p15 で未実装エラーになる）。
func New(mem cpu.Memory, cp15 Coprocessor) *Core {
	c := &Core{mem: mem, cp15: cp15}
	if f, ok := mem.(cpu.InstructionFetcher); ok {
		c.fetch32 = f.Fetch32
	} else {
		c.fetch32 = mem.Read32
	}
	c.prober, _ = mem.(cpu.Prober)
	return c
}

// Reset は電源投入相当。SVC モード・IRQ/FIQ 禁止・ARM state で pc から開始する。
// TODO: 本来のリセットはベクタ 0x00000000 へ飛ぶが、Device Emulator 同様に
// ローダーが決めたエントリポイントから直接開始する。実イメージで問題が出たら見直す。
func (c *Core) Reset(pc uint32) {
	histLen := c.histN
	*c = Core{mem: c.mem, cp15: c.cp15, fetch32: c.fetch32, prober: c.prober}
	c.SetHistory(histLen) // 履歴の設定は保つ（中身は空にする）
	c.cpsr = PSR(ModeSvc) | FlagI | FlagF
	c.regs[15] = pc
	if c.cp15 != nil {
		c.cp15.SetPrivileged(true)
	}
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
// Arch=true は「実機（ARM920T 構成）でも未定義命令例外になる」ことが
// 確かな命令で、Step がゲストに例外として配送する（WinCE は FPU 検出
// などで意図的に未定義命令を実行する）。Arch=false はエミュレータの
// 実装漏れの可能性があるため、例外にせず停止して気づけるようにする。
type UndefinedError struct {
	PC     uint32
	Word   uint32
	Reason string
	Arch   bool
}

func (e *UndefinedError) Error() string {
	return fmt.Sprintf("unimplemented/undefined instruction at PC=%08X: word=%08X (%s)", e.PC, e.Word, e.Reason)
}

// Step は 1 命令実行する。MMU 起因のアボート（cpu.AbortError）は ARM の
// 例外として配送して nil を返す。それ以外のエラー（未実装命令・未マップ
// 物理アドレス等）は PC を命令の位置に戻してエラーを返す
// （レジスタ・メモリの部分的な副作用までは巻き戻さない）。
func (c *Core) Step() error {
	if c.hist != nil {
		c.recordHistory()
	}
	// 割り込みは命令境界で受け付ける。FIQ が IRQ より優先（ARM ARM A2.6）。
	// 復帰先は「実行されなかった命令」なので LR = その PC+4
	// （ハンドラは SUBS pc, lr, #4 で戻る）。
	if c.fiq && c.cpsr&FlagF == 0 {
		c.enterException(VecFIQ, ModeFiq, c.regs[15]+4)
		return nil
	}
	if c.irq && c.cpsr&FlagI == 0 {
		c.enterException(VecIRQ, ModeIrq, c.regs[15]+4)
		return nil
	}

	if c.cpsr.T() {
		return c.stepThumb()
	}

	pc := c.regs[15]
	word, err := c.fetch32(pc)
	if err != nil {
		if isAbort(err) {
			// プリフェッチアボート。FSR/FAR はデータアボート専用なので更新しない
			//（ARM ARM: FAR is only updated for data aborts）。LR = PC+4。
			c.enterException(VecPabt, ModeAbt, pc+4)
			return nil
		}
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
		if derr := c.deliverExecError(err, pc, 4); derr != nil {
			if ue, ok := derr.(*UndefinedError); ok {
				ue.PC = pc
				ue.Word = word
			}
			c.regs[15] = pc
			return derr
		}
	}
	return nil
}

// deliverExecError は命令実行中のエラーのうち ARM 例外として配送できる
// ものを処理する。配送したら nil、できないもの（エミュレータ未実装等）は
// そのまま返す。instrLen は未定義例外の LR 計算用（ARM=4, Thumb=2）。
func (c *Core) deliverExecError(err error, pc, instrLen uint32) error {
	var ae *cpu.AbortError
	if errors.As(err, &ae) {
		// データアボート。FSR（ドメイン|ステータス）と FAR を更新して配送。
		// LR = PC+8（ARM/Thumb 共通。ハンドラは SUBS pc, lr, #8 で再実行できる）。
		if c.cp15 != nil {
			_ = c.cp15.Write(0, 5, 0, 0, uint32(ae.Domain)<<4|uint32(ae.Status))
			_ = c.cp15.Write(0, 6, 0, 0, ae.VA)
		}
		c.enterException(VecDabt, ModeAbt, pc+8)
		return nil
	}
	if ue, ok := err.(*UndefinedError); ok && ue.Arch {
		// 実機でも未定義例外になる命令: ゲストに配送する。
		// LR = 未定義命令の次（ARM ARM A2.6.4）。
		c.enterException(VecUndef, ModeUnd, pc+instrLen)
		return nil
	}
	return err
}

// isAbort は err が MMU 起因のアボートか（例外配送の対象か）を判定する。
func isAbort(err error) bool {
	var ae *cpu.AbortError
	return errors.As(err, &ae)
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
		if c.cp15 != nil {
			// MMU の権限チェック用に特権状態の変化を伝える（usr だけが非特権）。
			c.cp15.SetPrivileged(newMode != ModeUsr)
		}
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

// 例外ベクタオフセット。ベースは CP15 の V ビットで 0 または 0xFFFF0000
// （enterException が Coprocessor.VectorBase で解決する）。
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
	base := uint32(0)
	if c.cp15 != nil {
		base = c.cp15.VectorBase()
	}
	c.regs[15] = base | vector
}
