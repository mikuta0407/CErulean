package smdk2410

import "fmt"

// 入力 API（タッチ・キー）。UI やスクリプト再生（cmd、将来の gomobile）は
// 命令の合間にこれらを呼ぶ。決定論性は「どの命令数の時点で呼んだか」で
// 決まるので、再現が必要な呼び出し側は Steps() を基準にすること。

// タッチパネル（S3C2410 ADC/TS）の画面座標 → ADC 生値の変換。
//
// 根拠（2026-09、実イメージの touch.dll をトレースして確認した事実）:
// ドライバはキャリブレーションデータを使わず（レジストリの
// HARDWARE\DEVICEMAP\TOUCH には MaxCalError=7 しかない）、固定の一次式で
// 1/4 ピクセル単位の座標（画面 240×320 → 960×1280）に変換する:
//
//	X4 = (ADCDAT1 − 85) × 960 / 880        （0〜959 にクリップ）
//	Y4 = (1023 − ADCDAT0 − 105) × 1280 / 875（0〜1279 にクリップ）
//
// つまりパネルの X/Y と画面の軸は入れ替わっていて、画面 X は ADC の
// Y 測定値（ADCDAT1）、画面 Y は X 測定値（ADCDAT0）の反転から求まる。
// 割り算は定数の逆数掛け（smull）で、0 方向への切り捨て。
// ここではその逆変換として、ピクセル中心（4x+2）に対応する生値を返す。
const (
	touchScreenW = 240
	touchScreenH = 320

	touchD1Offset = 85  // ADCDAT1 のオフセット（画面 X 用）
	touchD1Span   = 880 // ADCDAT1 の範囲（960 に対応）
	touchD0Offset = 105 // 1023−ADCDAT0 のオフセット（画面 Y 用）
	touchD0Span   = 875 // 同範囲（1280 に対応）
)

// TouchScreenSize はタッチ座標の範囲（= LCD の解像度）。
func (m *Machine) TouchScreenSize() (w, h int) { return touchScreenW, touchScreenH }

// touchToRaw は画面ピクセル (x, y) を ADCDAT0（XPDATA）・ADCDAT1（YPDATA）
// に入る生値へ変換する。
func touchToRaw(x, y int) (xp, yp uint32) {
	x4 := 4*x + 2
	y4 := 4*y + 2
	yp = uint32(touchD1Offset + (x4*touchD1Span+960/2)/960)
	inv := touchD0Offset + (y4*touchD0Span+1280/2)/1280
	xp = uint32(1023 - inv)
	return xp, yp
}

func (m *Machine) touchCheck(x, y int) error {
	if x < 0 || y < 0 || x >= touchScreenW || y >= touchScreenH {
		return fmt.Errorf("touch position (%d,%d) outside %dx%d", x, y, touchScreenW, touchScreenH)
	}
	return nil
}

// TouchDown はペンを画面座標 (x, y) に下ろす。
func (m *Machine) TouchDown(x, y int) error {
	if err := m.touchCheck(x, y); err != nil {
		return err
	}
	xp, yp := touchToRaw(x, y)
	m.setPen(true, xp, yp)
	return nil
}

// TouchMove はペンを下ろしたまま (x, y) へ動かす。ペンが上がっていれば
// TouchDown と同じ。
func (m *Machine) TouchMove(x, y int) error { return m.TouchDown(x, y) }

// TouchUp はペンを上げる（位置は最後の値のまま）。
func (m *Machine) TouchUp() {
	m.syncTime()
	m.adc.SetPenUp()
	m.updateDeadline()
}

// TouchRaw はタッチパネルの ADC 生値（0〜1023）を直接与える（調査用）。
// down=false でペンアップ。
func (m *Machine) TouchRaw(down bool, rawX, rawY uint32) { m.setPen(down, rawX, rawY) }

// setPen は ADC にペンの状態を渡す。ADC は時間を持つので、溜めた仮想時間を
// 先に渡してから変える（run.go の timedDev と同じ理由）。
func (m *Machine) setPen(down bool, rawX, rawY uint32) {
	m.syncTime()
	m.adc.SetPen(down, rawX, rawY)
	m.updateDeadline()
}
