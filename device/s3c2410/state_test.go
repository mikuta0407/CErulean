package s3c2410

import (
	"testing"
	"time"

	"github.com/mikuta0407/cerulean/snapshot/snapshottest"
)

// 各デバイスの全フィールドが「保存する」か「配線・構成」かに分類されて
// いること（フィールド追加時の保存漏れ検出）。
func TestDeviceStateFields(t *testing.T) {
	snapshottest.CheckFields(t, Stub{}, []string{"regs"}, []string{"name", "forced"})
	snapshottest.CheckFields(t, INTC{},
		[]string{"srcpnd", "intmod", "intmsk", "priority", "intpnd", "intoffset", "subsrcpnd", "intsubmsk"},
		[]string{"update"})
	snapshottest.CheckFields(t, PWMTimer{},
		[]string{"tcfg0", "tcfg1", "tcon", "tcntb", "tcmpb", "cnt", "running"}, []string{"raise"})
	snapshottest.CheckFields(t, LCD{}, []string{"regs", "palette"}, nil)
	snapshottest.CheckFields(t, RTC{}, []string{"base", "elapsed", "rtccon", "other"}, []string{"pclkHz"})
	snapshottest.CheckFields(t, UART{}, []string{"ulcon", "ucon", "ufcon", "umcon", "ubrdiv"}, []string{"w"})
	snapshottest.CheckFields(t, DMAStub{}, []string{"stub"}, []string{"raise"})
}

func TestStubStateRoundTrip(t *testing.T) {
	src := NewStub("s", map[uint32]uint32{0x10: 5})
	src.Write(0x24, 4, 0xDEADBEEF)
	src.Write(0x08, 1, 0x7F)
	dst := NewStub("s", nil)
	snapshottest.RoundTrip(t, src, dst)
	for _, off := range []uint32{0x10, 0x24, 0x08} {
		if a, b := src.Read(off, 4), dst.Read(off, 4); a != b {
			t.Errorf("reg %X: %08X vs %08X", off, a, b)
		}
	}
}

func TestINTCStateRoundTrip(t *testing.T) {
	var calls int
	src := NewINTC(nil)
	src.Write(regINTMSK, 4, ^uint32(1<<IntTimer4))
	src.Write(regINTSUBMSK, 4, 0)
	src.Raise(IntTimer4)
	src.RaiseSub(SubTC)
	dst := NewINTC(func(irq, fiq bool) { calls++ })
	snapshottest.RoundTrip(t, src, dst)
	for off := uint32(0); off <= regINTSUBMSK; off += 4 {
		if a, b := src.Read(off, 4), dst.Read(off, 4); a != b {
			t.Errorf("reg %X: %08X vs %08X", off, a, b)
		}
	}
	if calls != 0 {
		t.Errorf("LoadState drove the IRQ line %d times; the CPU restores its own line state", calls)
	}
}

// タイマーは復元後も同じ時刻に満了すること（途中まで数えた残りを保つ）。
func TestTimerStateRoundTrip(t *testing.T) {
	var firedA, firedB []int
	a := NewPWMTimer(func(n int) { firedA = append(firedA, n) })
	a.Write(regTCFG0, 4, 0)
	a.Write(0x3C, 4, 100) // TCNTB4
	a.Write(regTCON, 4, 1<<21|1<<22)
	a.Write(regTCON, 4, 1<<20|1<<22) // start + auto reload
	a.Advance(150)
	b := NewPWMTimer(func(n int) { firedB = append(firedB, n) })
	snapshottest.RoundTrip(t, a, b)
	firedA = nil
	a.Advance(1000)
	b.Advance(1000)
	if len(firedA) == 0 || len(firedA) != len(firedB) {
		t.Errorf("fired %v vs %v", firedA, firedB)
	}
	if a.Read(0x40, 4) != b.Read(0x40, 4) {
		t.Errorf("TCNTO4 %d vs %d", a.Read(0x40, 4), b.Read(0x40, 4))
	}
}

func TestRTCStateRoundTrip(t *testing.T) {
	src := NewRTC(1000)
	src.SetTime(time.Date(2006, 1, 2, 15, 4, 5, 0, time.Local))
	src.Advance(12345)
	src.Write(0x50, 4, 0x42) // アラーム等（値保持）
	dst := NewRTC(1000)
	snapshottest.RoundTrip(t, src, dst)
	if !src.Now().Equal(dst.Now()) {
		t.Errorf("time %v vs %v", src.Now(), dst.Now())
	}
	if dst.Read(0x50, 4) != 0x42 {
		t.Errorf("other register not restored")
	}
}

func TestLCDUARTDMAStateRoundTrip(t *testing.T) {
	l := NewLCD()
	l.Write(regLCDCON1, 4, 0x6F9)
	l.Write(lcdPalBase+8, 4, 0xABC)
	l2 := NewLCD()
	snapshottest.RoundTrip(t, l, l2)
	if l2.Read(regLCDCON1, 4) != 0x6F9 || l2.Read(lcdPalBase+8, 4) != 0xABC {
		t.Error("LCD not restored")
	}

	u := NewUART(nil)
	u.Write(regUBRDIV, 4, 26)
	u2 := NewUART(nil)
	snapshottest.RoundTrip(t, u, u2)
	if u2.Read(regUBRDIV, 4) != 26 {
		t.Error("UART not restored")
	}

	d := NewDMAStub(nil)
	d.Write(0x10, 4, 0x1234)
	d2 := NewDMAStub(nil)
	snapshottest.RoundTrip(t, d, d2)
	if d2.Read(0x14, 4) != 0x1234 {
		t.Error("DMA not restored")
	}
}
