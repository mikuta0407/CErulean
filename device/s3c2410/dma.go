package s3c2410

import "github.com/mikuta0407/cerulean/bus"

// DMAStub は DMA コントローラ（データシート Ch.8、4 チャネル×0x40 間隔）の
// 最小スタブ。実転送は行わず、「起動された転送は即座に完了する」ように
// 見せる。オーディオ（IIS）ドライバのブートを通すのが目的。
//
// 各チャネルのレジスタ: DISRC 0x00 / DISRCC 0x04 / DIDST 0x08 / DIDSTC 0x0C /
// DCON 0x10 / DSTAT 0x14 / DCSRC 0x18 / DCDST 0x1C / DMASKTRIG 0x20。
//
// 挙動（2026-09 の実イメージ観察に基づく）:
//   - DSTAT の CURR_TC[19:0] は常に DCON の TC を返す。ドライバが起動直後に
//     「CURR_TC != 0（転送中）」をポーリングするため、0 だとハングする。
//   - DMASKTRIG の ON_OFF(bit1) が書かれたら、そのチャネルの完了割り込みを
//     即座に上げる。カーネルが DMA ISR の立てるフラグをスピンで待つため。
//     ON_OFF が bit1 なのは User's Manual Rev 1.1 の DMASKTRIGn で確認済み。
//
// TODO: 実 DMA（メモリ↔IIS 等の転送）はオーディオ対応時に実装する。
// 「CURR_TC == 0（完了）」のポーリングが現れたらこのモデルでは破綻する。
type DMAStub struct {
	stub *Stub

	// raise は完了割り込みの通知（machine が INTC の INT_DMAn へ配線する）。
	raise func(ch int)
}

var _ bus.Device = (*DMAStub)(nil)

func NewDMAStub(raise func(ch int)) *DMAStub {
	return &DMAStub{stub: NewStub("dma", nil), raise: raise}
}

func (d *DMAStub) Read(off uint32, size int) uint32 {
	// チャネル内オフセット 0x14 = DSTAT。対応する DCON の TC を返す。
	if off < 4*0x40 && off&0x3F == 0x14 {
		return d.stub.Read(off&^0x3F|0x10, 4) & 0xFFFFF
	}
	return d.stub.Read(off, size)
}

func (d *DMAStub) Write(off uint32, size int, v uint32) {
	d.stub.Write(off, size, v)
	// DMASKTRIG(+0x20) の ON_OFF: 転送即完了として割り込みを上げる。
	if off < 4*0x40 && off&0x3F == 0x20 && v&2 != 0 && d.raise != nil {
		d.raise(int(off >> 6))
	}
}
