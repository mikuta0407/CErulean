package s3c2410

import "github.com/mikuta0407/cerulean/bus"

// SPI は S3C2410 の SPI コントローラ（データシート Ch.22）の 2 チャネル分。
// チャネル n のレジスタは 0x20*n からの 6 本:
//
//	0x00 SPCON  SMOD[6:5] ENSCK[4] MSTR[3] CPOL[2] CPHA[1] TAGD[0]
//	0x04 SPSTA  DCOL[2] MULF[1] REDY[0]（読み出し専用）
//	0x08 SPPIN  ENMUL[2] KEEP[0]
//	0x0C SPPRE  プリスケーラ
//	0x10 SPTDAT 送信データ（書くと転送開始）
//	0x14 SPRDAT 受信データ
//
// モデル: マスターとして SPTDAT に書いた瞬間に 1 バイトの交換が完了する
// （転送時間は 0。REDY は常に 1）。相手は SPISlave で、チャネルに何も
// つながっていなければ 0 を受信する。SMOD=01（割り込みモード）なら転送
// 完了ごとに INT_SPIn を上げる。
//
// 常に REDY=1 にする理由: ブート時にドライバが SPSTA1 の REDY をポーリング
// するため（2026-09 に実測。以前は値保持スタブで REDY を立てていた）。
//
// レジスタ配置・SMOD（00 ポーリング/01 割り込み/10 DMA）・リセット値・
// INT_SPI0/1（22/29）は User's Manual Rev 1.1 の Ch.22・Ch.14 で確認済み。
// TODO: DMA モード（SMOD=10）とスレーブモード、TAGD（自動ガベージ送信）、
// DCOL/MULF は未実装。
type SPI struct {
	ch [2]spiChannel

	// raise は転送完了の割り込み（チャネル番号）を通知する（machine が配線）。
	raise func(ch int)
}

type spiChannel struct {
	spcon, sppin, sppre uint32
	rx                  uint32
	slave               SPISlave
}

// SPISlave は SPI バスの相手デバイス。Transfer は送信バイトを受け取り、
// 同時に返すバイトを返す（全二重で 1 バイト交換）。
type SPISlave interface {
	Transfer(tx byte) (rx byte)
}

var _ bus.Device = (*SPI)(nil)

const (
	regSPCON  = 0x00
	regSPSTA  = 0x04
	regSPPIN  = 0x08
	regSPPRE  = 0x0C
	regSPTDAT = 0x10
	regSPRDAT = 0x14

	spstaREDY = 1 << 0
	smodInt   = 1 // SPCON[6:5]=01: 割り込みモード
)

// NewSPI は raise で転送完了割り込みを通知する SPI を作る。
func NewSPI(raise func(ch int)) *SPI {
	s := &SPI{raise: raise}
	for i := range s.ch {
		// リセット値 0x02（KEEP[0]=0、予約 bit1 は「1 にすること」）。
		s.ch[i].sppin = 0x02
	}
	return s
}

// Attach はチャネル ch に相手デバイスをつなぐ。
func (s *SPI) Attach(ch int, slave SPISlave) { s.ch[ch].slave = slave }

func (s *SPI) Read(off uint32, size int) uint32 {
	n := int(off / 0x20)
	if n >= len(s.ch) {
		return 0
	}
	c := &s.ch[n]
	switch off % 0x20 &^ 3 {
	case regSPCON:
		return c.spcon
	case regSPSTA:
		return spstaREDY
	case regSPPIN:
		return c.sppin
	case regSPPRE:
		return c.sppre
	case regSPRDAT:
		return c.rx
	}
	return 0
}

func (s *SPI) Write(off uint32, size int, v uint32) {
	n := int(off / 0x20)
	if n >= len(s.ch) {
		return
	}
	c := &s.ch[n]
	switch off % 0x20 &^ 3 {
	case regSPCON:
		c.spcon = v & 0x7F
	case regSPPIN:
		c.sppin = v & 0x7
	case regSPPRE:
		c.sppre = v & 0xFF
	case regSPTDAT:
		c.rx = 0
		if c.slave != nil {
			c.rx = uint32(c.slave.Transfer(byte(v)))
		}
		if (c.spcon>>5)&3 == smodInt && s.raise != nil {
			s.raise(n)
		}
	}
}
