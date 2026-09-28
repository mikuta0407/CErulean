package s3c2410

import (
	"image/color"
	"testing"
)

// lcdSetup は TFT・bpp・解像度・フレームバッファを指定してレジスタを書く。
// ビット配置は lcd.go 冒頭の表どおり。
func lcdSetup(l *LCD, bppmode uint32, w, h int, base uint32, con5 uint32, offsize uint32) {
	pagewidth := uint32(w) // 16bpp: 1 ピクセル = 1 ハーフワード
	switch bppmode {
	case 0xD:
		pagewidth = uint32(w) * 2
	case 0xB:
		pagewidth = uint32(w) / 2
	}
	l.Write(regLCDCON1, 4, 3<<5|bppmode<<1|1) // TFT・ENVID=1
	l.Write(regLCDCON2, 4, uint32(h-1)<<14)
	l.Write(regLCDCON3, 4, uint32(w-1)<<8)
	l.Write(regLCDCON5, 4, con5)
	l.Write(regLCDSADDR1, 4, (base>>22)<<21|(base>>1)&0x1FFFFF)
	l.Write(regLCDSADDR3, 4, offsize<<11|pagewidth)
}

func TestLCDConfig(t *testing.T) {
	tests := []struct {
		name    string
		bppmode uint32
		w, h    int
		base    uint32
		con5    uint32
		offsize uint32
		want    LCDConfig
	}{
		{
			name: "WM5 想定 240x320 16bpp 565 HWSWP", bppmode: 0xC, w: 240, h: 320,
			base: 0x30100000, con5: 1<<11 | 1,
			want: LCDConfig{Enabled: true, TFT: true, BPP: 16, Width: 240, Height: 320,
				Base: 0x30100000, Stride: 480, HWSwap: true, FRM565: true, PageWidth: 240},
		},
		{
			name: "仮想画面（OFFSIZE あり）", bppmode: 0xC, w: 240, h: 320,
			base: 0x33F00000, offsize: 16,
			want: LCDConfig{Enabled: true, TFT: true, BPP: 16, Width: 240, Height: 320,
				Base: 0x33F00000, Stride: 512, PageWidth: 240, OffSize: 16},
		},
		{
			name: "24bpp BPP24BL", bppmode: 0xD, w: 640, h: 480,
			base: 0x31000000, con5: 1 << 12,
			want: LCDConfig{Enabled: true, TFT: true, BPP: 24, Width: 640, Height: 480,
				Base: 0x31000000, Stride: 2560, BPP24Low: true, PageWidth: 1280},
		},
		{
			name: "8bpp パレット", bppmode: 0xB, w: 320, h: 240, base: 0x30000000,
			want: LCDConfig{Enabled: true, TFT: true, BPP: 8, Width: 320, Height: 240,
				Base: 0x30000000, Stride: 320, PageWidth: 160},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			l := NewLCD()
			lcdSetup(l, tt.bppmode, tt.w, tt.h, tt.base, tt.con5, tt.offsize)
			if got := l.Config(); got != tt.want {
				t.Errorf("Config() =\n  %+v\nwant\n  %+v", got, tt.want)
			}
		})
	}
}

func TestLCDConfigDisabledAndSTN(t *testing.T) {
	l := NewLCD()
	l.Write(regLCDCON1, 4, 2<<5|0xC<<1) // PNRMODE=10（STN 8bit）・ENVID=0
	c := l.Config()
	if c.Enabled || c.TFT || c.BPP != 0 {
		t.Errorf("Config() = %+v, want disabled non-TFT with BPP 0", c)
	}
	if _, _, err := l.Frame(func(uint32) (uint32, error) { return 0, nil }); err == nil {
		t.Error("Frame on STN mode: want error")
	}
}

func TestLCDReadOnlyFields(t *testing.T) {
	l := NewLCD()
	l.Write(regLCDCON1, 4, 0xFFFFFFFF)
	if got := l.Read(regLCDCON1, 4); got&(0x3FF<<18) != 0 {
		t.Errorf("LINECNT must be read-only: LCDCON1=%08X", got)
	}
	l.Write(regLCDCON5, 4, 0xFFFFFFFF)
	if got := l.Read(regLCDCON5, 4); got&(0xF<<13) != 0 {
		t.Errorf("VSTATUS/HSTATUS must be read-only: LCDCON5=%08X", got)
	}
	l.Write(0x400+4*5, 4, 0xF800)
	if got := l.Read(0x400+4*5, 4); got != 0xF800 {
		t.Errorf("palette[5] = %X, want F800", got)
	}
}

// fakeMem は物理アドレス→ワードの疎なメモリ。
type fakeMem map[uint32]uint32

func (m fakeMem) read32(pa uint32) (uint32, error) { return m[pa], nil }

func TestLCDFrame16(t *testing.T) {
	red := color.RGBA{0xFF, 0, 0, 0xFF}
	green := color.RGBA{0, 0xFF, 0, 0xFF}
	blue := color.RGBA{0, 0, 0xFF, 0xFF}
	white := color.RGBA{0xFF, 0xFF, 0xFF, 0xFF}

	// 4x2 画像、ワード = [上位ハーフ | 下位ハーフ]。
	// 1 行目: 0x30000000 = F800(赤) | 07E0(緑)、0x30000004 = 001F(青) | FFFF(白)
	mem := fakeMem{
		0x30000000: 0xF800<<16 | 0x07E0,
		0x30000004: 0x001F<<16 | 0xFFFF,
		// 2 行目（OFFSIZE=2 ハーフワードで stride=12 バイト）
		0x3000000C: 0xFFFF<<16 | 0x001F,
	}
	tests := []struct {
		name string
		con5 uint32
		row0 [4]color.RGBA
	}{
		// スワップなし: 上位ハーフワードが左ピクセル。
		{"no swap", 1 << 11, [4]color.RGBA{red, green, blue, white}},
		// HWSWP: 下位ハーフワードが左ピクセル（LE の自然な並び）。
		{"HWSWP", 1<<11 | 1, [4]color.RGBA{green, red, white, blue}},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			l := NewLCD()
			lcdSetup(l, 0xC, 4, 2, 0x30000000, tt.con5, 2)
			img, _, err := l.Frame(mem.read32)
			if err != nil {
				t.Fatal(err)
			}
			for x, want := range tt.row0 {
				if got := img.RGBAAt(x, 0); got != want {
					t.Errorf("(%d,0) = %v, want %v", x, got, want)
				}
			}
			// 2 行目の先頭は stride 分ずれた位置から読まれる。
			want := white
			if tt.con5&1 != 0 {
				want = blue
			}
			if got := img.RGBAAt(0, 1); got != want {
				t.Errorf("(0,1) = %v, want %v", got, want)
			}
		})
	}
}

func TestLCDFrame24AndPalette(t *testing.T) {
	l := NewLCD()
	lcdSetup(l, 0xD, 2, 1, 0x30000000, 0, 0)
	img, _, err := l.Frame(fakeMem{0x30000000: 0x123456, 0x30000004: 0xFF000000}.read32)
	if err != nil {
		t.Fatal(err)
	}
	if got := img.RGBAAt(0, 0); got != (color.RGBA{0x12, 0x34, 0x56, 0xFF}) {
		t.Errorf("24bpp (0,0) = %v", got)
	}
	if got := img.RGBAAt(1, 0); got != (color.RGBA{0, 0, 0, 0xFF}) {
		t.Errorf("24bpp (1,0) = %v (上位 8 ビットは無視されるはず)", got)
	}

	// 8bpp: 1 ワードに 4 ピクセル、最上位バイトが左。
	l = NewLCD()
	lcdSetup(l, 0xB, 4, 1, 0x30000000, 1<<11, 0)
	l.Write(0x400+4*1, 4, 0xF800) // 1 = 赤
	l.Write(0x400+4*2, 4, 0x001F) // 2 = 青
	img, _, err = l.Frame(fakeMem{0x30000000: 0x01020001}.read32)
	if err != nil {
		t.Fatal(err)
	}
	want := []color.RGBA{{0xFF, 0, 0, 0xFF}, {0, 0, 0xFF, 0xFF}, {0, 0, 0, 0xFF}, {0xFF, 0, 0, 0xFF}}
	for x, w := range want {
		if got := img.RGBAAt(x, 0); got != w {
			t.Errorf("8bpp (%d,0) = %v, want %v", x, got, w)
		}
	}
}

func TestRGB565(t *testing.T) {
	tests := []struct {
		p     uint32
		is565 bool
		want  color.RGBA
	}{
		{0xFFFF, true, color.RGBA{0xFF, 0xFF, 0xFF, 0xFF}},
		{0x0000, true, color.RGBA{0, 0, 0, 0xFF}},
		{0x8410, true, color.RGBA{0x84, 0x82, 0x84, 0xFF}}, // 中間灰（ビット複製の確認）
		{0xFFFF, false, color.RGBA{0xFF, 0xFF, 0xFF, 0xFF}},
		{0xF800, false, color.RGBA{0xF8 | 0x3, 0, 0, 0xFF}}, // 5:5:5:I、I=0 → R=111110 → 0xFB
		{0x0001, false, color.RGBA{0x04 | 0, 0x04, 0x04, 0xFF}},
	}
	for _, tt := range tests {
		if got := rgb565(tt.p, tt.is565); got != tt.want {
			t.Errorf("rgb565(%04X, %v) = %v, want %v", tt.p, tt.is565, got, tt.want)
		}
	}
}
