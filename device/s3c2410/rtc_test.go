package s3c2410

import (
	"testing"
	"time"
)

func TestRTCRead(t *testing.T) {
	r := NewRTC(1000) // 1 秒 = 1000 ティック
	r.SetTime(time.Date(2006, 12, 31, 23, 59, 58, 0, time.UTC))
	check := func(label string, want map[uint32]uint32) {
		t.Helper()
		for off, w := range want {
			if got := r.Read(off, 4); got != w {
				t.Errorf("%s: reg %02X = %02X, want %02X", label, off, got, w)
			}
		}
	}
	// 2006-12-31 は日曜（BCDDAY=1）
	check("start", map[uint32]uint32{
		regBCDSEC: 0x58, regBCDMIN: 0x59, regBCDHOUR: 0x23,
		regBCDDATE: 0x31, regBCDDAY: 0x01, regBCDMON: 0x12, regBCDYEAR: 0x06,
	})
	r.Advance(999) // 1 秒未満: 変わらない
	check("+0.999s", map[uint32]uint32{regBCDSEC: 0x58})
	r.Advance(1001) // 計 2 秒: 年をまたぐ
	check("+2s", map[uint32]uint32{
		regBCDSEC: 0x00, regBCDMIN: 0x00, regBCDHOUR: 0x00,
		regBCDDATE: 0x01, regBCDDAY: 0x02, regBCDMON: 0x01, regBCDYEAR: 0x07,
	})
}

func TestRTCWrite(t *testing.T) {
	r := NewRTC(1000)
	r.SetTime(time.Date(2006, 1, 2, 3, 4, 5, 0, time.UTC))

	r.Write(regBCDHOUR, 4, 0x15) // RTCEN=0: 無視される
	if got := r.Read(regBCDHOUR, 4); got != 0x03 {
		t.Errorf("write with RTCEN=0 changed hour to %02X", got)
	}

	r.Write(regRTCCON, 4, 1)
	r.Write(regBCDYEAR, 4, 0x08)
	r.Write(regBCDMON, 4, 0x02)
	r.Write(regBCDDATE, 4, 0x29) // 2008 はうるう年
	r.Write(regBCDHOUR, 4, 0x15)
	want := time.Date(2008, 2, 29, 15, 4, 5, 0, time.UTC)
	if got := r.Now(); !got.Equal(want) {
		t.Errorf("Now() = %v, want %v", got, want)
	}
	r.Advance(1000)
	if got := r.Read(regBCDSEC, 4); got != 0x06 {
		t.Errorf("sec after 1s = %02X, want 06", got)
	}
}

func TestBCD(t *testing.T) {
	for _, v := range []int{0, 9, 10, 45, 99} {
		if got := fromBCD(toBCD(v)); got != v {
			t.Errorf("BCD round trip %d -> %d", v, got)
		}
	}
	if toBCD(59) != 0x59 {
		t.Errorf("toBCD(59) = %X", toBCD(59))
	}
}
