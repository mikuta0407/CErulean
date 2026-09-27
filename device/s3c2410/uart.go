// Package s3c2410 は Samsung S3C2410 SoC の周辺機器を実装する。
// レジスタ仕様の根拠は S3C2410 データシート。
package s3c2410

import (
	"io"

	"github.com/mikuta0407/cerulean/bus"
)

// UART は S3C2410 の UART 1 チャネル分（マイルストーン1では送信のみ）。
// レジスタオフセット（データシート Ch.11、リトルエンディアン時）:
//
//	0x00 ULCON   0x04 UCON    0x08 UFCON   0x0C UMCON
//	0x10 UTRSTAT 0x14 UERSTAT 0x18 UFSTAT  0x1C UMSTAT
//	0x20 UTXH    0x24 URXH    0x28 UBRDIV
type UART struct {
	w io.Writer // 送信バイトの出力先（CLI では標準出力）

	// 設定レジスタは書かれた値を保持するだけ（ボーレート等は動作に影響しない）。
	ulcon, ucon, ufcon, umcon, ubrdiv uint32
}

var _ bus.Device = (*UART)(nil)

// NewUART は送信データを w に流す UART を作る。
func NewUART(w io.Writer) *UART { return &UART{w: w} }

const (
	regULCON   = 0x00
	regUCON    = 0x04
	regUFCON   = 0x08
	regUMCON   = 0x0C
	regUTRSTAT = 0x10
	regUERSTAT = 0x14
	regUFSTAT  = 0x18
	regUMSTAT  = 0x1C
	regUTXH    = 0x20
	regURXH    = 0x24
	regUBRDIV  = 0x28
)

func (u *UART) Read(off uint32, size int) uint32 {
	switch off &^ 3 {
	case regULCON:
		return u.ulcon
	case regUCON:
		return u.ucon
	case regUFCON:
		return u.ufcon
	case regUMCON:
		return u.umcon
	case regUTRSTAT:
		// bit2: transmitter empty, bit1: TX buffer empty。
		// 送信は即時に w へ書くので常に空。bit0（RX ready）は 0 = 受信なし。
		return 0x6
	case regUFSTAT:
		return 0 // FIFO 空
	case regUERSTAT, regUMSTAT, regURXH:
		return 0 // 受信は未実装（TODO: マイルストーン2以降でキー入力等に対応）
	case regUBRDIV:
		return u.ubrdiv
	}
	return 0
}

func (u *UART) Write(off uint32, size int, v uint32) {
	switch off &^ 3 {
	case regULCON:
		u.ulcon = v
	case regUCON:
		u.ucon = v
	case regUFCON:
		u.ufcon = v
	case regUMCON:
		u.umcon = v
	case regUBRDIV:
		u.ubrdiv = v
	case regUTXH:
		if u.w != nil {
			// 送信ホールディングレジスタ: 下位 8 ビットを即時出力する。
			// エラーは無視（標準出力への書き込み失敗で CPU を止めたくない）。
			_, _ = u.w.Write([]byte{byte(v)})
		}
	}
}
