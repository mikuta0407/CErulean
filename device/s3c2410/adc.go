package s3c2410

import "github.com/mikuta0407/cerulean/bus"

// ADC は S3C2410 の A/D コンバータとタッチスクリーン I/F（データシート Ch.16）。
//
// レジスタ（オフセット）:
//
//	0x00 ADCCON  ECFLG[15](RO) PRSCEN[14] PRSCVL[13:6] SEL_MUX[5:3]
//	             STDBM[2] READ_START[1] ENABLE_START[0]
//	0x04 ADCTSC  UD_SEN[8] YM_SEN[7] YP_SEN[6] XM_SEN[5] XP_SEN[4] PULL_UP[3]
//	             AUTO_PST[2] XY_PST[1:0]（00 なし/01 X 測定/10 Y 測定/11 割り込み待ち）
//	0x08 ADCDLY
//	0x0C ADCDAT0 UPDOWN[15](0=ペンダウン) AUTO_PST[14] XY_PST[13:12] XPDATA[9:0]
//	0x10 ADCDAT1 UPDOWN[15] AUTO_PST[14] XY_PST[13:12] YPDATA[9:0]
//
// 割り込み: INT_ADC のサブソース INT_TC（ペン検出）と INT_ADC（変換完了）。
//
// モデル:
//   - ペンの状態と「その位置での X/Y の ADC 生値（10 ビット）」は machine が
//     SetPen で与える。画面座標→生値の変換は machine の責務（パネルの
//     向き・範囲はボード依存のため）。
//   - 変換は開始から conversionTicks 後に完了し、ECFLG を立てて INT_ADC を
//     上げる。AUTO_PST=1 なら X・Y を両方、XY_PST=01/10 なら片方を測る。
//   - 割り込み待ちモード（XY_PST=11）で、UD_SEN=0 ならペンダウン、UD_SEN=1 なら
//     ペンアップを検出して INT_TC を上げる。
//
// UD_SEN（bit8）について: S3C2410 のデータシートでは bit8 は予約だったはず
// （UD_SEN は S3C2440 の機能として記憶している）。しかし実イメージの touch.dll
// （Device Emulator 用）は、サンプリング後に ADCTSC=0x1D3（bit8=1 の割り込み
// 待ち）を書き、ペンアップを INT_TC だけで検出する（タイマー駆動の
// サンプリング経路は UPDOWN を一切読まない。2026-09 にコードをトレースして
// 確認）。Device Emulator の ADC は bit8 を S3C2440 の UD_SEN と同じ意味で
// 実装していると判断し、それに合わせる。
//
// 根拠: S3C2410X User's Manual Rev 1.1 の Ch.16（レジスタ配置・リセット値・
// 変換時間・ADCDLY は確認済み）。ADCTSC の bit8 はマニュアルでは「予約（0 に
// すること）」で、上記の UD_SEN の意味は Device Emulator 独自と判断した。
//
// 変換時間: 1 回の測定 = ADCDLY の遅延（変換中は PCLK で数える。Figure 16-3）
// ＋ 5 ADC クロック（ADC クロック = PCLK/(PRSCVL+1)。「50MHz/(49+1)=1MHz、
// 5 サイクルで 5us」の記述）。自動（逐次）モードは X と Y の 2 回分。
//
// TODO: 未確認・未実装:
//   - プリスケーラ無効（PRSCEN=0）時の ADC クロック（ここでは PCLK として 5 PCLK）
//   - 割り込み待ちモードでペンダウン中に ADCDLY 間隔で INT_TC を繰り返す動作
//     （ADCDLY の説明にある。touch.dll は Timer3 でサンプリングするので未実装）
//   - 割り込み待ちに入った時点で既に検出対象の状態なら INT_TC を出す（レベル扱い）
//     のは判断。こうしないと、サンプリング中（ADCTSC=0xDC の間）にペンが上がった
//     場合にペンアップを取りこぼす。
type ADC struct {
	adccon, adctsc, adcdly uint32
	dat0, dat1             uint32 // 変換結果（下位 10 ビット）

	converting int64 // 変換完了までの残り PCLK ティック（0 = 変換中でない）
	ecflg      bool

	penDown    bool
	rawX, rawY uint32

	// raiseSub は INT_TC/INT_ADC のサブソースを上げる（machine が INTC へ配線）。
	raiseSub func(sub uint)
}

var _ bus.Device = (*ADC)(nil)

const (
	regADCCON  = 0x00
	regADCTSC  = 0x04
	regADCDLY  = 0x08
	regADCDAT0 = 0x0C
	regADCDAT1 = 0x10

	adcconECFLG       = 1 << 15
	adcconPRSCEN      = 1 << 14
	adcconREADSTART   = 1 << 1
	adcconENABLESTART = 1 << 0

	adctscAutoPST = 1 << 2
	adctscUDSEN   = 1 << 8
	xyPSTNone     = 0
	xyPSTX        = 1
	xyPSTY        = 2
	xyPSTWait     = 3

	datUPDOWN = 1 << 15
)

// NewADC は raiseSub でサブソース（SubTC/SubADC）を通知する ADC を作る。
func NewADC(raiseSub func(sub uint)) *ADC {
	return &ADC{
		adccon:   0x3FC4, // リセット値（マニュアルで確認済み）
		adctsc:   0x58,
		adcdly:   0xFF,
		raiseSub: raiseSub,
	}
}

func (a *ADC) xyPST() uint32 { return a.adctsc & 3 }

// SetPen はペンの状態と、その位置で X/Y を測ったときの生値（0〜1023）を
// 設定する。割り込み待ちモードで検出対象の変化（UD_SEN 参照）が起きたら
// INT_TC を上げる。
func (a *ADC) SetPen(down bool, rawX, rawY uint32) {
	was := a.penDown
	a.penDown = down
	a.rawX, a.rawY = rawX&0x3FF, rawY&0x3FF
	if down != was && a.tcCondition() {
		a.raise(SubTC)
	}
}

// tcCondition は「割り込み待ちモードで、検出対象のペン状態にある」か。
func (a *ADC) tcCondition() bool {
	if a.xyPST() != xyPSTWait {
		return false
	}
	wantUp := a.adctsc&adctscUDSEN != 0
	return a.penDown != wantUp
}

func (a *ADC) raise(sub uint) {
	if a.raiseSub != nil {
		a.raiseSub(sub)
	}
}

// conversionTicks は変換開始から完了までの PCLK ティック数（型コメント参照）。
func (a *ADC) conversionTicks() int64 {
	conv := int64(5)
	if a.adccon&adcconPRSCEN != 0 {
		conv = int64((a.adccon>>6)&0xFF+1) * 5
	}
	one := int64(a.adcdly) + conv
	if a.adctsc&adctscAutoPST != 0 {
		return 2 * one // X と Y を順に測る
	}
	return one
}

func (a *ADC) start() {
	a.ecflg = false
	a.converting = a.conversionTicks()
}

// NextEvent は変換完了まであと何ティックか（変換中でなければ NoEvent）。
// PWMTimer.NextEvent と同じく machine の Advance まとめ用。
func (a *ADC) NextEvent() int64 {
	if a.converting == 0 {
		return NoEvent
	}
	return max(a.converting, 1)
}

// Advance は仮想時間を PCLK ティック数だけ進める（変換の完了判定）。
func (a *ADC) Advance(ticks int64) {
	if a.converting == 0 {
		return
	}
	a.converting -= ticks
	if a.converting > 0 {
		return
	}
	a.converting = 0
	a.ecflg = true
	switch {
	case a.adctsc&adctscAutoPST != 0:
		a.dat0, a.dat1 = a.rawX, a.rawY
	case a.xyPST() == xyPSTX:
		a.dat0 = a.rawX
	case a.xyPST() == xyPSTY:
		a.dat1 = a.rawY
	default:
		// 通常の A/D 変換（SEL_MUX のチャネル）。アナログ入力は未接続として 0。
		// TODO: バッテリー電圧等を AIN で測るドライバが現れたら値を与える。
		a.dat0 = 0
	}
	a.raise(SubADC)
}

// datStatus は ADCDAT0/1 の上位ビット（UPDOWN・AUTO_PST・XY_PST）。
func (a *ADC) datStatus() uint32 {
	v := (a.adctsc & adctscAutoPST) << 12 // bit2 → bit14
	v |= a.xyPST() << 12
	if !a.penDown {
		v |= datUPDOWN
	}
	return v
}

func (a *ADC) Read(off uint32, size int) uint32 {
	switch off &^ 3 {
	case regADCCON:
		v := a.adccon &^ (adcconECFLG | adcconENABLESTART)
		if a.ecflg {
			v |= adcconECFLG
		}
		return v
	case regADCTSC:
		return a.adctsc
	case regADCDLY:
		return a.adcdly
	case regADCDAT0:
		v := a.datStatus() | a.dat0
		if a.adccon&adcconREADSTART != 0 && a.converting == 0 {
			a.start() // READ_START: 読み出しで次の変換を開始
		}
		return v
	case regADCDAT1:
		return a.datStatus() | a.dat1
	}
	return 0
}

func (a *ADC) Write(off uint32, size int, v uint32) {
	switch off &^ 3 {
	case regADCCON:
		a.adccon = v &^ adcconECFLG
		if v&adcconENABLESTART != 0 {
			a.start() // ENABLE_START は開始後に自動で 0 に戻る（Read 参照）
		}
	case regADCTSC:
		was := a.tcCondition()
		a.adctsc = v & 0x1FF
		if !was && a.tcCondition() {
			a.raise(SubTC)
		}
	case regADCDLY:
		a.adcdly = v & 0xFFFF
	}
}

// SetPenUp はペンを上げる（生値は最後の値のまま）。
func (a *ADC) SetPenUp() { a.SetPen(false, a.rawX, a.rawY) }
