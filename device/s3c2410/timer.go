package s3c2410

import "github.com/mikuta0407/cerulean/bus"

// PWMTimer は S3C2410 の PWM タイマー（データシート Ch.10）のうち、
// カウントダウンと割り込み生成だけを実装する（TOUT 出力・PWM 波形・
// デッドゾーンは未実装。WinCE はシステムティックに Timer4 を使う）。
//
// 時間は machine が命令数ベースの仮想時間で進める（Advance に PCLK
// ティック数を渡す。ユーザー確認済み 2026-09）。各タイマーの残り時間は
// PCLK ティック単位（カウント値 × スケール）で保持する。
type PWMTimer struct {
	tcfg0, tcfg1, tcon uint32
	tcntb              [5]uint32
	tcmpb              [4]uint32 // Timer4 に TCMPB はない

	cnt     [5]int64 // 残り PCLK ティック（<=0 は停止中/満了）
	running [5]bool

	// raise は満了した Timer n の割り込みを通知する（machine が INTC へ配線）。
	raise func(timer int)
}

var _ bus.Device = (*PWMTimer)(nil)

func NewPWMTimer(raise func(timer int)) *PWMTimer {
	return &PWMTimer{raise: raise}
}

const (
	regTCFG0 = 0x00
	regTCFG1 = 0x04
	regTCON  = 0x08
	// 以降 TCNTBn/TCMPBn/TCNTOn（Timer4 は TCNTB4=0x3C, TCNTO4=0x40）
)

// tconBits は TCON 内の Timer n の (start, manual, autoreload) ビット位置。
// Timer0: [3:0]、Timer1-3: [11:8]/[15:12]/[19:16]、Timer4: [22:20]
// （Timer4 はインバータビットがないためリロードが +2）。
func tconBits(n int) (start, manual, reload uint32) {
	switch n {
	case 0:
		return 1 << 0, 1 << 1, 1 << 3
	case 4:
		return 1 << 20, 1 << 21, 1 << 22
	default:
		shift := uint(4 + 4*n) // Timer1=8, 2=12, 3=16
		return 1 << shift, 1 << (shift + 1), 1 << (shift + 3)
	}
}

// scale は Timer n の 1 カウントあたりの PCLK ティック数
// （(プリスケーラ+1) × 分周比）。
func (t *PWMTimer) scale(n int) int64 {
	presc := t.tcfg0 & 0xFF // Timer0/1
	if n >= 2 {
		presc = (t.tcfg0 >> 8) & 0xFF
	}
	mux := (t.tcfg1 >> (4 * uint(n))) & 0xF
	div := int64(2) << (mux & 3)
	if mux >= 4 {
		// 外部 TCLK / TOUT 入力は未対応。
		// TODO: 必要になったら実装する。当面は最大分周(1/16)として扱う。
		div = 16
	}
	return int64(presc+1) * div
}

// period は自動リロード 1 周期分の PCLK ティック数。
// TODO: 実機の周期が TCNTB か TCNTB+1 かはデータシートの波形図で要確認。
// ここでは TCNTB カウント（0 なら 1）としている。ずれても 1 カウント。
func (t *PWMTimer) period(n int) int64 {
	c := int64(t.tcntb[n])
	if c <= 0 {
		c = 1
	}
	return c * t.scale(n)
}

// Advance は仮想時間を PCLK ティック数だけ進め、満了したタイマーの
// 割り込みを上げる。
func (t *PWMTimer) Advance(ticks int64) {
	for n := 0; n < 5; n++ {
		if !t.running[n] {
			continue
		}
		t.cnt[n] -= ticks
		for t.cnt[n] <= 0 {
			if t.raise != nil {
				t.raise(n)
			}
			_, _, reload := tconBits(n)
			if t.tcon&reload == 0 {
				t.running[n] = false // ワンショット: 停止
				t.cnt[n] = 0
				break
			}
			t.cnt[n] += t.period(n) // 自動リロード
		}
	}
}

func (t *PWMTimer) Read(off uint32, size int) uint32 {
	switch off &^ 3 {
	case regTCFG0:
		return t.tcfg0
	case regTCFG1:
		return t.tcfg1
	case regTCON:
		return t.tcon
	}
	if n, kind, ok := timerReg(off &^ 3); ok {
		switch kind {
		case 0: // TCNTB
			return t.tcntb[n]
		case 1: // TCMPB
			return t.tcmpb[n]
		default: // TCNTO: 現在値をスケールから逆算
			if s := t.scale(n); s > 0 && t.cnt[n] > 0 {
				return uint32(t.cnt[n] / s)
			}
			return 0
		}
	}
	return 0
}

// timerReg は 0x0C 以降のレジスタを（タイマー番号, 種別）に変換する。
// 種別: 0=TCNTB, 1=TCMPB, 2=TCNTO。
func timerReg(off uint32) (n int, kind int, ok bool) {
	if off < 0x0C || off > 0x40 {
		return 0, 0, false
	}
	i := (off - 0x0C) / 4 // 0..13
	if i < 12 {           // Timer0-3: 3 レジスタずつ
		return int(i / 3), int(i % 3), true
	}
	// Timer4: TCNTB4(0x3C), TCNTO4(0x40)
	if i == 12 {
		return 4, 0, true
	}
	return 4, 2, true
}

func (t *PWMTimer) Write(off uint32, size int, v uint32) {
	switch off &^ 3 {
	case regTCFG0:
		t.tcfg0 = v
		return
	case regTCFG1:
		t.tcfg1 = v
		return
	case regTCON:
		old := t.tcon
		t.tcon = v
		for n := 0; n < 5; n++ {
			start, manual, _ := tconBits(n)
			if v&manual != 0 {
				// マニュアルアップデート: TCNTB を内部カウンタにロード。
				t.cnt[n] = int64(t.tcntb[n]) * t.scale(n)
			}
			switch {
			case v&start != 0 && old&start == 0:
				// スタート。マニュアルアップデートを経ずに開始された場合は
				// リロード値から数える。
				if t.cnt[n] <= 0 {
					t.cnt[n] = t.period(n)
				}
				t.running[n] = true
			case v&start == 0:
				t.running[n] = false
			}
		}
		return
	}
	if n, kind, ok := timerReg(off &^ 3); ok {
		switch kind {
		case 0:
			t.tcntb[n] = v
		case 1:
			if n < 4 {
				t.tcmpb[n] = v
			}
		}
		// TCNTO は読み出し専用
	}
}
