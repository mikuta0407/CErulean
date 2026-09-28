package s3c2410

import (
	"fmt"
	"image"
	"image/color"

	"github.com/mikuta0407/cerulean/bus"
)

// LCD は S3C2410 の LCD コントローラ（データシート Ch.15）。
//
// 表示の生成（ピクセルクロック・同期信号）はエミュレートせず、レジスタ
// から「フレームバッファがどこにあって、何ピクセル × 何ビットか」を
// 解釈するだけにする。画面が必要になった時点で Frame が RAM から画像を作る。
//
// 対応範囲: TFT（PNRMODE=11）の 16bpp と 24bpp、および TFT 1/2/4/8bpp の
// パレットモード。STN は未対応（Device Emulator 構成は TFT のはず）。
//
// レジスタ（オフセット）:
//
//	0x00 LCDCON1   LINECNT[27:18] CLKVAL[17:8] MMODE[7] PNRMODE[6:5] BPPMODE[4:1] ENVID[0]
//	0x04 LCDCON2   VBPD[31:24] LINEVAL[23:14] VFPD[13:6] VSPW[5:0]
//	0x08 LCDCON3   HBPD[25:19] HOZVAL[18:8] HFPD[7:0]
//	0x0C LCDCON4   MVAL[15:8] HSPW[7:0]
//	0x10 LCDCON5   BPP24BL[12] FRM565[11] ... BSWP[1] HWSWP[0]
//	0x14 LCDSADDR1 LCDBANK[29:21]=A[30:22] LCDBASEU[20:0]=A[21:1]
//	0x18 LCDSADDR2 LCDBASEL[20:0]=A[21:1]（フレーム終端）
//	0x1C LCDSADDR3 OFFSIZE[21:11] PAGEWIDTH[10:0]（いずれもハーフワード単位）
//	0x400〜0x7FC   パレット（256 エントリ）
//
// 上記ビット配置と、16bpp で HWSWP=1 なら下位ハーフワードが左ピクセル
// （Figure 15-5）であることは User's Manual Rev 1.1 の Ch.15 で確認済み。
type LCD struct {
	regs    [0x70 / 4]uint32 // 0x00〜0x6C
	palette [256]uint32
}

var _ bus.Device = (*LCD)(nil)

func NewLCD() *LCD { return &LCD{} }

const (
	regLCDCON1   = 0x00
	regLCDCON2   = 0x04
	regLCDCON3   = 0x08
	regLCDCON4   = 0x0C
	regLCDCON5   = 0x10
	regLCDSADDR1 = 0x14
	regLCDSADDR2 = 0x18
	regLCDSADDR3 = 0x1C
	lcdPalBase   = 0x400
	lcdPalEnd    = 0x800
)

func (l *LCD) Read(off uint32, size int) uint32 {
	off &^= 3
	switch {
	case off >= lcdPalBase && off < lcdPalEnd:
		return l.palette[(off-lcdPalBase)/4]
	case off < uint32(len(l.regs))*4:
		// LINECNT（LCDCON1[27:18]）と VSTATUS/HSTATUS（LCDCON5[16:13]）は
		// 走査位置を示す読み出し専用フィールド。走査はエミュレートしないので 0
		// （= 1 行目・VSYNC 期間）を返す。
		// TODO: ドライバが垂直同期待ちでこれをポーリングしてハングしたら、
		// 仮想時間から走査位置を生成する。
		return l.regs[off/4]
	}
	return 0
}

func (l *LCD) Write(off uint32, size int, v uint32) {
	off &^= 3
	switch {
	case off >= lcdPalBase && off < lcdPalEnd:
		l.palette[(off-lcdPalBase)/4] = v
	case off < uint32(len(l.regs))*4:
		switch off {
		case regLCDCON1:
			v &^= 0x3FF << 18 // LINECNT は読み出し専用
		case regLCDCON5:
			v &^= 0xF << 13 // VSTATUS/HSTATUS は読み出し専用
		}
		l.regs[off/4] = v
	}
}

// LCDConfig はレジスタから解釈した表示設定。
type LCDConfig struct {
	Enabled   bool   // ENVID（映像出力）
	TFT       bool   // PNRMODE == 11
	BPP       int    // 1/2/4/8/16/24（未対応モードは 0）
	Width     int    // HOZVAL+1
	Height    int    // LINEVAL+1
	Base      uint32 // フレームバッファ先頭の物理アドレス
	Stride    uint32 // 1 行のバイト数（PAGEWIDTH+OFFSIZE、ハーフワード単位 ×2）
	HWSwap    bool   // HWSWP: 16bpp でワード内のハーフワード順を入れ替える
	ByteSwap  bool   // BSWP: ワード内のバイト順を入れ替える
	BPP24Low  bool   // BPP24BL: 24bpp で有効データが下位 24 ビット側か
	FRM565    bool   // FRM565: 16bpp を 5:6:5 として扱う（0 なら 5:5:5:I）
	PageWidth uint32 // ハーフワード単位（診断用）
	OffSize   uint32 // ハーフワード単位（診断用）
}

func (c LCDConfig) String() string {
	return fmt.Sprintf("enabled=%v tft=%v %dx%d %dbpp base=%08X stride=%d hwswp=%v bswp=%v frm565=%v",
		c.Enabled, c.TFT, c.Width, c.Height, c.BPP, c.Base, c.Stride, c.HWSwap, c.ByteSwap, c.FRM565)
}

// Config は現在のレジスタ値を解釈する。
func (l *LCD) Config() LCDConfig {
	con1 := l.regs[regLCDCON1/4]
	con2 := l.regs[regLCDCON2/4]
	con3 := l.regs[regLCDCON3/4]
	con5 := l.regs[regLCDCON5/4]
	sa1 := l.regs[regLCDSADDR1/4]
	sa3 := l.regs[regLCDSADDR3/4]

	c := LCDConfig{
		Enabled:   con1&1 != 0,
		TFT:       (con1>>5)&3 == 3,
		Width:     int((con3>>8)&0x7FF) + 1,
		Height:    int((con2>>14)&0x3FF) + 1,
		HWSwap:    con5&(1<<0) != 0,
		ByteSwap:  con5&(1<<1) != 0,
		FRM565:    con5&(1<<11) != 0,
		BPP24Low:  con5&(1<<12) != 0,
		PageWidth: sa3 & 0x7FF,
		OffSize:   (sa3 >> 11) & 0x7FF,
	}
	if c.TFT {
		// TFT の BPPMODE: 1000=1, 1001=2, 1010=4, 1011=8, 1100=16, 1101=24bpp。
		switch (con1 >> 1) & 0xF {
		case 0x8:
			c.BPP = 1
		case 0x9:
			c.BPP = 2
		case 0xA:
			c.BPP = 4
		case 0xB:
			c.BPP = 8
		case 0xC:
			c.BPP = 16
		case 0xD:
			c.BPP = 24
		}
	}
	// フレームバッファ先頭: LCDBANK が A[30:22]、LCDBASEU が A[21:1]。
	c.Base = (sa1>>21&0x1FF)<<22 | (sa1&0x1FFFFF)<<1
	c.Stride = (c.PageWidth + c.OffSize) * 2
	return c
}

// Frame はフレームバッファを RGBA 画像に変換する。read32 は物理アドレスの
// ワード読み出し（machine が RAM を渡す）。表示無効・未対応モードなら error。
func (l *LCD) Frame(read32 func(pa uint32) (uint32, error)) (*image.RGBA, LCDConfig, error) {
	c := l.Config()
	if !c.TFT || c.BPP == 0 {
		return nil, c, fmt.Errorf("lcd: unsupported mode (%v)", c)
	}
	img := image.NewRGBA(image.Rect(0, 0, c.Width, c.Height))
	for y := 0; y < c.Height; y++ {
		row := c.Base + uint32(y)*c.Stride
		for x := 0; x < c.Width; x++ {
			px, err := l.pixel(c, row, x, read32)
			if err != nil {
				return nil, c, err
			}
			img.SetRGBA(x, y, px)
		}
	}
	return img, c, nil
}

// word は BSWP を反映したワード読み出し。
func word(c LCDConfig, pa uint32, read32 func(uint32) (uint32, error)) (uint32, error) {
	w, err := read32(pa &^ 3)
	if err != nil {
		return 0, err
	}
	if c.ByteSwap {
		w = w>>24 | (w>>8)&0xFF00 | (w<<8)&0xFF0000 | w<<24
	}
	return w, nil
}

// pixel は行先頭 row から x 番目のピクセル。
//
// メモリ上のピクセル順（データシートの「メモリデータフォーマット」表）:
// スワップなしでは 1 ワード内の最上位側が画面左のピクセルになる
// （ビッグエンディアン的な並び）。WinCE（リトルエンディアン）は 16bpp で
// HWSWP=1 にして、下位ハーフワードを左ピクセルにする想定。
func (l *LCD) pixel(c LCDConfig, row uint32, x int, read32 func(uint32) (uint32, error)) (color.RGBA, error) {
	switch c.BPP {
	case 24:
		// 24bpp は 1 ピクセル 1 ワード。BPP24BL=0 なら [23:0] が RGB888、
		// 1 なら [31:8]。
		w, err := word(c, row+uint32(x)*4, read32)
		if err != nil {
			return color.RGBA{}, err
		}
		if c.BPP24Low {
			w >>= 8
		}
		return color.RGBA{R: uint8(w >> 16), G: uint8(w >> 8), B: uint8(w), A: 0xFF}, nil
	case 16:
		w, err := word(c, row+uint32(x)*2, read32)
		if err != nil {
			return color.RGBA{}, err
		}
		// スワップなし: 左ピクセル（偶数 x）が D[31:16]。HWSWP=1 なら D[15:0]。
		first := x&1 == 0
		var p uint32
		if first != c.HWSwap {
			p = w >> 16
		} else {
			p = w & 0xFFFF
		}
		return rgb565(p, c.FRM565), nil
	default:
		// パレットモード: 1 ワードに 32/BPP 個、最上位側が左ピクセル。
		perWord := 32 / c.BPP
		w, err := word(c, row+uint32(x/perWord)*4, read32)
		if err != nil {
			return color.RGBA{}, err
		}
		shift := uint(32 - c.BPP*(x%perWord+1))
		idx := (w >> shift) & (1<<uint(c.BPP) - 1)
		// TODO: パレットエントリの形式は TPAL/LCDCON5 FRM565 に従う
		// （5:6:5 か 5:5:5:I）。当面 16bpp と同じ解釈をする。
		return rgb565(l.palette[idx]&0xFFFF, c.FRM565), nil
	}
}

// rgb565 は 16 ビット画素を 8 ビット RGB に展開する。
// FRM565=0 の 5:5:5:I 形式は R[15:11] G[10:6] B[5:1] I[0]（I は各色の
// 最下位ビット）。下位ビットは上位ビットの複製で埋めて 0〜255 に広げる。
func rgb565(p uint32, is565 bool) color.RGBA {
	r5 := (p >> 11) & 0x1F
	b5 := (p >> 0) & 0x1F
	var g8 uint8
	if is565 {
		g6 := (p >> 5) & 0x3F
		g8 = uint8(g6<<2 | g6>>4)
	} else {
		// 5:5:5:I: G は [10:6]、B は [5:1]、I を各色の最下位として付ける。
		i := p & 1
		g5 := (p >> 6) & 0x1F
		b5 = (p >> 1) & 0x1F
		r6, g6, b6 := r5<<1|i, g5<<1|i, b5<<1|i
		return color.RGBA{R: uint8(r6<<2 | r6>>4), G: uint8(g6<<2 | g6>>4), B: uint8(b6<<2 | b6>>4), A: 0xFF}
	}
	return color.RGBA{R: uint8(r5<<3 | r5>>2), G: g8, B: uint8(b5<<3 | b5>>2), A: 0xFF}
}
