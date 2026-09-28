package s3c2410

import "github.com/mikuta0407/cerulean/bus"

// INTC は S3C2410 の割り込みコントローラ（データシート Ch.14）。
//
// デバイス → Raise/RaiseSub → SRCPND/SUBSRCPND → マスク・モード判定 →
// INTPND/INTOFFSET 確定 → update コールバックで CPU の IRQ/FIQ 線を駆動する。
//
// 簡略化（ユーザー確認済み 2026-09）: PRIORITY レジスタの回転アービトレーション
// は実装せず、固定優先度（ビット番号の小さい順）で選択する。
// TODO: 実機依存の挙動が問題になったら ARB_SEL/ARB_MODE を実装する。
type INTC struct {
	srcpnd, intmod, intmsk, priority uint32
	intpnd, intoffset                uint32
	subsrcpnd, intsubmsk             uint32

	// update は IRQ/FIQ 線のレベル変化を CPU に伝える（machine が配線する）。
	update func(irq, fiq bool)
}

var _ bus.Device = (*INTC)(nil)

// 割り込みソース番号（SRCPND のビット位置。データシート Table 14-2）。
const (
	IntEINT0  = 0
	IntTick   = 8
	IntWDT    = 9
	IntTimer0 = 10
	IntTimer1 = 11
	IntTimer2 = 12
	IntTimer3 = 13
	IntTimer4 = 14
	IntUART2  = 15
	IntLCD    = 16
	IntDMA0   = 17
	IntDMA1   = 18
	IntDMA2   = 19
	IntDMA3   = 20
	IntSPI0   = 22 // TODO: ビット番号はデータシートと要照合
	IntUART1  = 23
	IntUART0  = 28
	IntSPI1   = 29
	IntRTC    = 30
	IntADC    = 31
)

// SUBSRCPND のビット位置（UART の送受信・エラー、ADC）。
const (
	SubRXD0 = 0
	SubTXD0 = 1
	SubERR0 = 2
	SubRXD1 = 3
	SubTXD1 = 4
	SubERR1 = 5
	SubRXD2 = 6
	SubTXD2 = 7
	SubERR2 = 8
	SubTC   = 9
	SubADC  = 10
)

const (
	regSRCPND    = 0x00
	regINTMOD    = 0x04
	regINTMSK    = 0x08
	regPRIORITY  = 0x0C
	regINTPND    = 0x10
	regINTOFFSET = 0x14
	regSUBSRCPND = 0x18
	regINTSUBMSK = 0x1C
)

// NewINTC は update で IRQ/FIQ 線を通知する INTC を作る。
// リセット値: INTMSK/INTSUBMSK は全マスク（データシート）。
func NewINTC(update func(irq, fiq bool)) *INTC {
	return &INTC{
		intmsk:    0xFFFFFFFF,
		intsubmsk: 0x7FF,
		update:    update,
	}
}

// Raise は割り込みソース src（ビット番号）のペンディングを立てる。
// エッジ相当: 立てるだけで、クリアはハンドラの SRCPND 書き込みが行う。
func (ic *INTC) Raise(src uint) {
	ic.srcpnd |= 1 << (src & 31)
	ic.recompute()
}

// RaiseSub は UART/ADC のサブソースを立てる。
func (ic *INTC) RaiseSub(sub uint) {
	ic.subsrcpnd |= 1 << (sub & 31)
	ic.recompute()
}

// subToSrc はサブソースの束をメインソースのビットに反映する。
// サブが立っている限り SRCPND 側は立て直される（クリアしてもサブが
// 残っていれば再セット）。
func (ic *INTC) subToSrc() {
	pend := ic.subsrcpnd &^ ic.intsubmsk
	if pend&0x007 != 0 {
		ic.srcpnd |= 1 << IntUART0
	}
	if pend&0x038 != 0 {
		ic.srcpnd |= 1 << IntUART1
	}
	if pend&0x1C0 != 0 {
		ic.srcpnd |= 1 << IntUART2
	}
	if pend&0x600 != 0 {
		ic.srcpnd |= 1 << IntADC
	}
}

// recompute はペンディング状態から INTPND/INTOFFSET と IRQ/FIQ 線を求め直す。
func (ic *INTC) recompute() {
	ic.subToSrc()

	fiq := ic.srcpnd & ic.intmod // FIQ は INTMSK の影響を受けない
	irqPend := ic.srcpnd &^ ic.intmsk &^ ic.intmod

	if irqPend != 0 {
		// 固定優先度: ビット番号の小さい順（PRIORITY は未実装）。
		off := uint32(0)
		for irqPend&(1<<off) == 0 {
			off++
		}
		ic.intpnd = 1 << off
		ic.intoffset = off
	} else {
		ic.intpnd = 0
		// INTOFFSET は最後の値を保持する（データシート: INTPND クリアで更新）。
	}

	if ic.update != nil {
		ic.update(ic.intpnd != 0, fiq != 0)
	}
}

func (ic *INTC) Read(off uint32, size int) uint32 {
	switch off &^ 3 {
	case regSRCPND:
		return ic.srcpnd
	case regINTMOD:
		return ic.intmod
	case regINTMSK:
		return ic.intmsk
	case regPRIORITY:
		return ic.priority
	case regINTPND:
		return ic.intpnd
	case regINTOFFSET:
		return ic.intoffset
	case regSUBSRCPND:
		return ic.subsrcpnd
	case regINTSUBMSK:
		return ic.intsubmsk
	}
	return 0
}

func (ic *INTC) Write(off uint32, size int, v uint32) {
	switch off &^ 3 {
	case regSRCPND: // 1 を書いたビットをクリア
		ic.srcpnd &^= v
	case regINTMOD:
		ic.intmod = v
	case regINTMSK:
		ic.intmsk = v
	case regPRIORITY:
		ic.priority = v // 保持のみ（固定優先度で代用）
	case regINTPND: // 1 を書いたビットをクリア
		ic.intpnd &^= v
	case regINTOFFSET:
		// 書き込み不可レジスタ。無視。
	case regSUBSRCPND:
		ic.subsrcpnd &^= v
	case regINTSUBMSK:
		ic.intsubmsk = v
	}
	ic.recompute()
}
