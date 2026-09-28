package smdk2410

import "testing"

// driverX4/driverY4 は touch.dll の座標変換（0x0153170C）をトレースした
// 命令列どおりに再現したもの（定数の逆数掛け・算術シフト・負の補正・
// クリップ）。入力は 4 サンプルの平均（ここでは同一値なので生値そのもの）。
func driverX4(d1 uint32) int32 {
	r3 := int32(d1&0x3FF) - 0x55
	r4 := r3 * 960
	hi := int32((int64(r4) * int64(int32(0x094F2095))) >> 32)
	v := hi >> 5
	v += int32(uint32(v) >> 31)
	return clamp(v, 960)
}

func driverY4(d0 uint32) int32 {
	inv := int32(0x3FF - d0&0x3FF)
	r3 := inv - 0x69
	lr := r3 * 1280
	hi := int32((int64(lr) * int64(int32(0x2572FB07))) >> 32)
	v := hi >> 7
	v += int32(uint32(v) >> 31)
	return clamp(v, 1280)
}

func clamp(v, n int32) int32 {
	if v < 0 {
		return 0
	}
	if v >= n {
		return n - 1
	}
	return v
}

// 全ピクセルについて、touchToRaw の生値をドライバの式に通すと同じ
// ピクセル（1/4 単位の座標 ÷4）に戻ること。
func TestTouchToRawRoundTrip(t *testing.T) {
	for x := 0; x < touchScreenW; x++ {
		for y := 0; y < touchScreenH; y++ {
			xp, yp := touchToRaw(x, y)
			if xp > 1023 || yp > 1023 {
				t.Fatalf("(%d,%d): raw out of range %d,%d", x, y, xp, yp)
			}
			gx, gy := driverX4(yp)/4, driverY4(xp)/4
			if int(gx) != x || int(gy) != y {
				t.Fatalf("(%d,%d) -> raw XP=%d YP=%d -> driver (%d,%d)", x, y, xp, yp, gx, gy)
			}
		}
	}
}

// 代表点（画面の四隅と中央）の生値。軸の入れ替えと Y の反転を明示する。
func TestTouchToRawCorners(t *testing.T) {
	tests := []struct {
		x, y   int
		xp, yp uint32
	}{
		{0, 0, 1023 - 105 - 1, 85 + 2},      // 左上: XP 大・YP 小
		{239, 0, 1023 - 105 - 1, 85 + 878},  // 右上: YP 大
		{0, 319, 1023 - 105 - 874, 85 + 2},  // 左下: XP 小
		{120, 160, 1023 - 105 - 439, 85 + 442},
	}
	for _, tt := range tests {
		xp, yp := touchToRaw(tt.x, tt.y)
		if xp != tt.xp || yp != tt.yp {
			t.Errorf("touchToRaw(%d,%d) = XP %d YP %d, want %d %d", tt.x, tt.y, xp, yp, tt.xp, tt.yp)
		}
	}
}

func TestTouchRange(t *testing.T) {
	m, err := New(nil)
	if err != nil {
		t.Fatal(err)
	}
	for _, p := range [][2]int{{-1, 0}, {0, -1}, {240, 0}, {0, 320}} {
		if err := m.TouchDown(p[0], p[1]); err == nil {
			t.Errorf("TouchDown(%d,%d) accepted", p[0], p[1])
		}
	}
	if err := m.TouchDown(239, 319); err != nil {
		t.Error(err)
	}
}
