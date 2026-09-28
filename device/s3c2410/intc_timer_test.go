package s3c2410

import "testing"

// lineRec は update コールバックの記録。
type lineRec struct {
	irq, fiq bool
}

func newTestINTC() (*INTC, *lineRec) {
	rec := &lineRec{}
	ic := NewINTC(func(irq, fiq bool) { rec.irq, rec.fiq = irq, fiq })
	return ic, rec
}

func TestINTCMaskAndDeliver(t *testing.T) {
	ic, rec := newTestINTC()

	// リセット時は全マスク: Raise しても IRQ 線は立たない
	ic.Raise(IntTimer4)
	if rec.irq {
		t.Fatal("IRQ asserted while masked")
	}
	if got := ic.Read(regSRCPND, 4); got != 1<<IntTimer4 {
		t.Errorf("SRCPND = %08X", got)
	}

	// マスク解除 → IRQ 線が立ち、INTPND/INTOFFSET が確定
	ic.Write(regINTMSK, 4, ^uint32(1<<IntTimer4))
	if !rec.irq || rec.fiq {
		t.Fatalf("after unmask: irq=%v fiq=%v", rec.irq, rec.fiq)
	}
	if got := ic.Read(regINTOFFSET, 4); got != IntTimer4 {
		t.Errorf("INTOFFSET = %d, want %d", got, IntTimer4)
	}
	if got := ic.Read(regINTPND, 4); got != 1<<IntTimer4 {
		t.Errorf("INTPND = %08X", got)
	}

	// ハンドラのクリア手順: SRCPND → INTPND に 1 を書く
	ic.Write(regSRCPND, 4, 1<<IntTimer4)
	ic.Write(regINTPND, 4, 1<<IntTimer4)
	if rec.irq {
		t.Error("IRQ still asserted after clear")
	}
	if got := ic.Read(regSRCPND, 4); got != 0 {
		t.Errorf("SRCPND after clear = %08X", got)
	}
}

func TestINTCFixedPriority(t *testing.T) {
	ic, rec := newTestINTC()
	ic.Write(regINTMSK, 4, 0) // 全部マスク解除
	ic.Raise(IntUART0)        // 28
	ic.Raise(IntTimer0)       // 10 ← 小さい方が勝つ
	if got := ic.Read(regINTOFFSET, 4); got != IntTimer0 {
		t.Errorf("INTOFFSET = %d, want %d (固定優先度)", got, IntTimer0)
	}
	// Timer0 をクリアすると UART0 が選ばれる
	ic.Write(regSRCPND, 4, 1<<IntTimer0)
	if got := ic.Read(regINTOFFSET, 4); got != IntUART0 {
		t.Errorf("INTOFFSET after clear = %d, want %d", got, IntUART0)
	}
	if !rec.irq {
		t.Error("IRQ should stay asserted while UART0 pending")
	}
}

func TestINTCFIQ(t *testing.T) {
	ic, rec := newTestINTC()
	ic.Write(regINTMOD, 4, 1<<IntTimer1) // Timer1 を FIQ に
	ic.Raise(IntTimer1)
	// FIQ は INTMSK に関係なく立ち、IRQ 側（INTPND）には現れない
	if !rec.fiq || rec.irq {
		t.Errorf("irq=%v fiq=%v, want fiq only", rec.irq, rec.fiq)
	}
	if got := ic.Read(regINTPND, 4); got != 0 {
		t.Errorf("INTPND = %08X, want 0 (FIQ source)", got)
	}
}

func TestINTCSubSource(t *testing.T) {
	ic, rec := newTestINTC()
	ic.Write(regINTMSK, 4, ^uint32(1<<IntUART1))
	ic.Write(regINTSUBMSK, 4, ^uint32(1<<SubRXD1))

	ic.RaiseSub(SubRXD1)
	if !rec.irq {
		t.Fatal("sub source should assert IRQ via INT_UART1")
	}
	if got := ic.Read(regINTOFFSET, 4); got != IntUART1 {
		t.Errorf("INTOFFSET = %d, want %d", got, IntUART1)
	}

	// SRCPND だけクリアしてもサブが残っていれば立て直される
	ic.Write(regSRCPND, 4, 1<<IntUART1)
	if got := ic.Read(regSRCPND, 4); got != 1<<IntUART1 {
		t.Errorf("SRCPND = %08X, want regenerated from sub", got)
	}
	// サブをクリアすれば落ちる
	ic.Write(regSUBSRCPND, 4, 1<<SubRXD1)
	ic.Write(regSRCPND, 4, 1<<IntUART1)
	if rec.irq {
		t.Error("IRQ still asserted after sub+src clear")
	}
}

// ---- PWM タイマー ----

func TestTimer4PeriodicInterrupt(t *testing.T) {
	var fired []int
	tm := NewPWMTimer(func(n int) { fired = append(fired, n) })

	// PCLK/2、プリスケーラ 0 → 1 カウント = 2 PCLK。TCNTB4=100 → 周期 200 PCLK。
	tm.Write(regTCFG0, 4, 0)
	tm.Write(regTCFG1, 4, 0)
	tm.Write(0x3C, 4, 100)            // TCNTB4
	tm.Write(regTCON, 4, 1<<21)       // Timer4 マニュアルアップデート
	tm.Write(regTCON, 4, 1<<20|1<<22) // スタート + 自動リロード

	tm.Advance(199)
	if len(fired) != 0 {
		t.Fatalf("fired too early: %v", fired)
	}
	tm.Advance(1)
	if len(fired) != 1 || fired[0] != 4 {
		t.Fatalf("fired = %v, want [4]", fired)
	}
	// 自動リロードで周期的に発火する
	tm.Advance(400)
	if len(fired) != 3 {
		t.Fatalf("fired = %v, want 3 total", fired)
	}
}

func TestTimerOneShotStops(t *testing.T) {
	var fired []int
	tm := NewPWMTimer(func(n int) { fired = append(fired, n) })
	tm.Write(0x0C, 4, 10)      // TCNTB0
	tm.Write(regTCON, 4, 1<<1) // Timer0 マニュアルアップデート
	tm.Write(regTCON, 4, 1<<0) // スタート（自動リロードなし）
	tm.Advance(1000)
	if len(fired) != 1 || fired[0] != 0 {
		t.Fatalf("fired = %v, want [0] (one-shot)", fired)
	}
}

func TestTimerTCNTOReadback(t *testing.T) {
	tm := NewPWMTimer(nil)
	tm.Write(0x3C, 4, 100)
	tm.Write(regTCON, 4, 1<<21)
	tm.Write(regTCON, 4, 1<<20|1<<22)
	tm.Advance(60) // 1 カウント = 2 PCLK → 30 カウント経過
	if got := tm.Read(0x40, 4); got != 70 {
		t.Errorf("TCNTO4 = %d, want 70", got)
	}
}

func TestTimerPrescalerScale(t *testing.T) {
	var fired []int
	tm := NewPWMTimer(func(n int) { fired = append(fired, n) })
	// Timer2: プリスケーラ1 = 9 (÷10)、mux 1/4 → 1 カウント = 40 PCLK
	tm.Write(regTCFG0, 4, 9<<8)
	tm.Write(regTCFG1, 4, 1<<8)
	tm.Write(0x24, 4, 5) // TCNTB2 → 周期 200 PCLK
	tm.Write(regTCON, 4, 1<<13)
	tm.Write(regTCON, 4, 1<<12|1<<15)
	tm.Advance(199)
	if len(fired) != 0 {
		t.Fatalf("fired too early: %v", fired)
	}
	tm.Advance(1)
	if len(fired) != 1 || fired[0] != 2 {
		t.Fatalf("fired = %v, want [2]", fired)
	}
}
