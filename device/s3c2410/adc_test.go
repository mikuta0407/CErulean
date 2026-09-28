package s3c2410

import "testing"

type subLog []uint

func (l *subLog) raise(s uint) { *l = append(*l, s) }

func TestADCPenDownInterrupt(t *testing.T) {
	tests := []struct {
		name   string
		adctsc uint32
		setup  func(a *ADC) // ペン操作
		wantTC int
	}{
		{"wait down, pen down", 0xD3, func(a *ADC) { a.SetPen(true, 1, 2) }, 1},
		{"wait down, down then up", 0xD3, func(a *ADC) { a.SetPen(true, 1, 2); a.SetPen(false, 0, 0) }, 1},
		{"wait down, move while down", 0xD3, func(a *ADC) { a.SetPen(true, 1, 2); a.SetPen(true, 3, 4) }, 1},
		// ペンアップ状態でアップ検出待ちに入った時点の 1 回だけ（ダウンでは出ない）
		{"wait up (UD_SEN), pen down", 0x1D3, func(a *ADC) { a.SetPen(true, 1, 2) }, 1},
		{"no-op mode, pen down", 0xD0, func(a *ADC) { a.SetPen(true, 1, 2) }, 0},
		{"X mode, pen down", 0xD1, func(a *ADC) { a.SetPen(true, 1, 2) }, 0},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			var log subLog
			a := NewADC(log.raise)
			a.Write(regADCTSC, 4, tt.adctsc)
			tt.setup(a)
			n := 0
			for _, s := range log {
				if s == SubTC {
					n++
				}
			}
			if n != tt.wantTC {
				t.Errorf("INT_TC raised %d times, want %d (log %v)", n, tt.wantTC, log)
			}
		})
	}
}

// 既にペンダウンの状態で割り込み待ちモードに入ると INT_TC が出る。
func TestADCEnterWaitWhilePenDown(t *testing.T) {
	var log subLog
	a := NewADC(log.raise)
	a.SetPen(true, 1, 2)
	a.Write(regADCTSC, 4, 0xD3)
	if len(log) != 1 || log[0] != SubTC {
		t.Errorf("log = %v, want [SubTC]", log)
	}
}

// touch.dll の実際の手順: ダウン検出 → サンプリング（0xDC）→ bit8=1 で
// 割り込み待ち（0x1D3）→ ペンアップで INT_TC。
func TestADCPenUpInterrupt(t *testing.T) {
	var log subLog
	a := NewADC(log.raise)
	a.Write(regADCTSC, 4, 0xD3)
	a.SetPen(true, 1, 2)
	a.Write(regADCTSC, 4, 0xDC)
	a.Write(regADCTSC, 4, 0x1D3)
	if len(log) != 1 {
		t.Fatalf("after down: log = %v, want one INT_TC", log)
	}
	a.SetPen(false, 0, 0)
	if len(log) != 2 || log[1] != SubTC {
		t.Errorf("pen up in UD_SEN wait mode: log = %v, want second INT_TC", log)
	}
}

// サンプリング中（割り込み待ちでない間）にペンが上がっても、bit8=1 の
// 割り込み待ちに入った時点で INT_TC が出る（取りこぼさない）。
func TestADCPenUpDuringSampling(t *testing.T) {
	var log subLog
	a := NewADC(log.raise)
	a.SetPen(true, 1, 2)
	a.Write(regADCTSC, 4, 0xDC)
	a.SetPen(false, 0, 0)
	if len(log) != 0 {
		t.Fatalf("INT_TC raised outside wait mode: %v", log)
	}
	a.Write(regADCTSC, 4, 0x1D3)
	if len(log) != 1 || log[0] != SubTC {
		t.Errorf("log = %v, want INT_TC on entering up-detect wait mode", log)
	}
}

func TestADCConversion(t *testing.T) {
	tests := []struct {
		name             string
		adctsc           uint32
		wantDat0, wantD1 uint32 // 下位 10 ビット
		wantTicks        int64  // 変換時間（ADCDLY=100、PRSCVL=49 → 1 回 100+250）
	}{
		{"auto sequential X/Y", 0x0C, 0x123, 0x2AB, 700},
		{"X only", 0x69, 0x123, 0, 350},
		{"Y only", 0x9A, 0, 0x2AB, 350},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			var log subLog
			a := NewADC(log.raise)
			a.SetPen(true, 0x123, 0x2AB)
			a.Write(regADCTSC, 4, tt.adctsc)
			a.Write(regADCDLY, 4, 100)
			a.Write(regADCCON, 4, adcconPRSCEN|49<<6|adcconENABLESTART)
			if v := a.Read(regADCCON, 4); v&adcconECFLG != 0 || v&adcconENABLESTART != 0 {
				t.Fatalf("ADCCON right after start = %08X (want ECFLG=0, ENABLE_START cleared)", v)
			}
			a.Advance(tt.wantTicks - 1)
			if a.Read(regADCCON, 4)&adcconECFLG != 0 || len(log) != 0 {
				t.Fatal("conversion completed too early")
			}
			a.Advance(1)
			if a.Read(regADCCON, 4)&adcconECFLG == 0 {
				t.Fatal("ECFLG not set after conversion")
			}
			if len(log) != 1 || log[0] != SubADC {
				t.Errorf("interrupts = %v, want [SubADC]", log)
			}
			d0, d1 := a.Read(regADCDAT0, 4), a.Read(regADCDAT1, 4)
			if d0&0x3FF != tt.wantDat0 || d1&0x3FF != tt.wantD1 {
				t.Errorf("ADCDAT0/1 = %08X/%08X, want data %03X/%03X", d0, d1, tt.wantDat0, tt.wantD1)
			}
			if d0&datUPDOWN != 0 {
				t.Error("UPDOWN says pen up while down")
			}
			// 状態ビット: AUTO_PST[14]・XY_PST[13:12] は ADCTSC の写し。
			if st := (d0 >> 12) & 7; st != tt.adctsc&7 {
				t.Errorf("ADCDAT0 status bits = %03b, want %03b", st, tt.adctsc&7)
			}
		})
	}
}

func TestADCPenUpFlag(t *testing.T) {
	a := NewADC(nil)
	if a.Read(regADCDAT0, 4)&datUPDOWN == 0 || a.Read(regADCDAT1, 4)&datUPDOWN == 0 {
		t.Error("UPDOWN should be 1 (up) initially")
	}
	a.SetPen(true, 0, 0)
	if a.Read(regADCDAT0, 4)&datUPDOWN != 0 {
		t.Error("UPDOWN should be 0 while pen is down")
	}
}

func TestADCReadStart(t *testing.T) {
	a := NewADC(nil)
	a.Write(regADCDLY, 4, 10)
	a.Write(regADCCON, 4, adcconREADSTART) // プリスケーラ無効: 10+5 ティック
	a.Read(regADCDAT0, 4)
	a.Advance(15)
	if a.Read(regADCCON, 4)&adcconECFLG == 0 {
		t.Error("reading ADCDAT0 with READ_START did not start a conversion")
	}
}

// StableRead は Read と同じ値を返し、読み出しで変換が始まる場合
// （READ_START 有効時の ADCDAT0）だけ断る。
func TestADCStableRead(t *testing.T) {
	a := NewADC(func(uint) {})
	a.Write(regADCCON, 4, 1) // ENABLE_START
	for _, off := range []uint32{regADCCON, regADCTSC, regADCDLY, regADCDAT0, regADCDAT1} {
		v, ok := a.StableRead(off, 4)
		if !ok || v != a.Read(off, 4) {
			t.Errorf("StableRead(%X) = %X,%v, Read = %X", off, v, ok, a.Read(off, 4))
		}
	}
	// 変換中は ECFLG=0、完了後は 1（値は変換完了のイベントでだけ変わる）。
	if v, _ := a.StableRead(regADCCON, 4); v&adcconECFLG != 0 {
		t.Error("ECFLG set during conversion")
	}
	a.Advance(a.NextEvent())
	if v, _ := a.StableRead(regADCCON, 4); v&adcconECFLG == 0 {
		t.Error("ECFLG not set after conversion")
	}
	a.Write(regADCCON, 4, adcconREADSTART)
	if _, ok := a.StableRead(regADCDAT0, 4); ok {
		t.Error("ADCDAT0 with READ_START starts a conversion; must not be stable")
	}
	if a.converting != 0 {
		t.Error("StableRead started a conversion")
	}
}
