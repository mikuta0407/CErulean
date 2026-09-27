package arm

import "testing"

// ---- 乗算命令エンコードヘルパー ----

// mul: MUL/MLA。a=true で MLA（+Rn）。
func mulEnc(s, a bool, rd, rn, rs, rm uint32) uint32 {
	w := condAL<<28 | rd<<16 | rn<<12 | rs<<8 | 9<<4 | rm
	if a {
		w |= 1 << 21
	}
	if s {
		w |= 1 << 20
	}
	return w
}

// mulLong: UMULL/UMLAL/SMULL/SMLAL。
func mulLongEnc(s, signed, a bool, rdHi, rdLo, rs, rm uint32) uint32 {
	w := condAL<<28 | 1<<23 | rdHi<<16 | rdLo<<12 | rs<<8 | 9<<4 | rm
	if signed {
		w |= 1 << 22
	}
	if a {
		w |= 1 << 21
	}
	if s {
		w |= 1 << 20
	}
	return w
}

// swpEnc: SWP/SWPB。
func swpEnc(byteXfer bool, rn, rd, rm uint32) uint32 {
	w := condAL<<28 | 1<<24 | rn<<16 | rd<<12 | 9<<4 | rm
	if byteXfer {
		w |= 1 << 22
	}
	return w
}

func TestMulMla(t *testing.T) {
	tests := []struct {
		name      string
		s, a      bool
		rmV, rsV  uint32
		rnV       uint32
		want      uint32
		flagsIn   string
		wantFlags string
	}{
		{"MUL 基本", false, false, 6, 7, 0, 42, "", ""},
		{"MUL 符号は関係ない(mod 2^32)", false, false, 0xFFFFFFFF, 3, 0, 0xFFFFFFFD, "", ""},
		{"MLA 加算", false, true, 5, 4, 100, 120, "", ""},
		{"MULS N", true, false, 0x80000000, 1, 0, 0x80000000, "", "N"},
		{"MULS Z", true, false, 0, 123, 0, 0, "", "Z"},
		// ARMv4 の MULS で C は UNPREDICTABLE。本実装は「不変」なので保存される。
		{"MULS C/V 不変", true, false, 2, 3, 0, 6, "CV", "CV"},
		{"MLAS でも同様", true, true, 2, 3, 4, 10, "C", "C"},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			c, mem := newTestCore()
			setFlags(c, tt.flagsIn)
			c.SetReg(1, tt.rmV)
			c.SetReg(2, tt.rsV)
			c.SetReg(3, tt.rnV)
			mustStep(t, c, mem, mulEnc(tt.s, tt.a, 4, 3, 2, 1))
			if got := c.Reg(4); got != tt.want {
				t.Errorf("rd = %08X, want %08X", got, tt.want)
			}
			if got := flagsOf(c.CPSR()); got != tt.wantFlags {
				t.Errorf("flags = %q, want %q", got, tt.wantFlags)
			}
		})
	}
}

func TestMulLong(t *testing.T) {
	tests := []struct {
		name      string
		signed, a bool
		s         bool
		rmV, rsV  uint32
		hiIn      uint32
		loIn      uint32
		wantHi    uint32
		wantLo    uint32
		wantFlags string
	}{
		{"UMULL 基本", false, false, false, 0xFFFFFFFF, 0xFFFFFFFF, 0, 0, 0xFFFFFFFE, 0x00000001, ""},
		{"UMULL 小さい値", false, false, false, 1000, 1000, 0, 0, 0, 1000000, ""},
		{"SMULL 負×正", true, false, false, 0xFFFFFFFF /* -1 */, 5, 0, 0, 0xFFFFFFFF, 0xFFFFFFFB, ""},
		{"SMULL 負×負", true, false, false, 0xFFFFFFFE /* -2 */, 0xFFFFFFFD /* -3 */, 0, 0, 0, 6, ""},
		{"UMLAL 加算", false, true, false, 2, 3, 1, 0xFFFFFFFF, 2, 5, ""},
		{"SMLAL 加算", true, true, false, 0xFFFFFFFF, 1, 0, 5, 0, 4, ""},
		{"UMULLS Z", false, false, true, 0, 12345, 0, 0, 0, 0, "Z"},
		{"SMULLS N (bit63)", true, false, true, 0xFFFFFFFF, 1, 0, 0, 0xFFFFFFFF, 0xFFFFFFFF, "N"},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			c, mem := newTestCore()
			c.SetReg(1, tt.rmV)
			c.SetReg(2, tt.rsV)
			c.SetReg(4, tt.hiIn)
			c.SetReg(3, tt.loIn)
			mustStep(t, c, mem, mulLongEnc(tt.s, tt.signed, tt.a, 4, 3, 2, 1))
			if hi, lo := c.Reg(4), c.Reg(3); hi != tt.wantHi || lo != tt.wantLo {
				t.Errorf("{hi,lo} = {%08X,%08X}, want {%08X,%08X}", hi, lo, tt.wantHi, tt.wantLo)
			}
			if got := flagsOf(c.CPSR()); got != tt.wantFlags {
				t.Errorf("flags = %q, want %q", got, tt.wantFlags)
			}
		})
	}
}

func TestSwp(t *testing.T) {
	c, mem := newTestCore()
	if err := mem.Write32(0x2000, 0x11223344); err != nil {
		t.Fatal(err)
	}
	c.SetReg(1, 0x2000)     // Rn: アドレス
	c.SetReg(2, 0xAABBCCDD) // Rm: 書き込む値
	mustStep(t, c, mem, swpEnc(false, 1, 3, 2))
	if got := c.Reg(3); got != 0x11223344 {
		t.Errorf("SWP: rd = %08X, want 11223344", got)
	}
	if got, _ := mem.Read32(0x2000); got != 0xAABBCCDD {
		t.Errorf("SWP: mem = %08X, want AABBCCDD", got)
	}
}

func TestSwpUnalignedRotate(t *testing.T) {
	// 非アラインアドレスのロード値は LDR と同じ回転。ストアはアラインされる。
	c, mem := newTestCore()
	if err := mem.Write32(0x2000, 0x11223344); err != nil {
		t.Fatal(err)
	}
	c.SetReg(1, 0x2001)
	c.SetReg(2, 0xAABBCCDD)
	mustStep(t, c, mem, swpEnc(false, 1, 3, 2))
	if got := c.Reg(3); got != ror(0x11223344, 8) {
		t.Errorf("rd = %08X, want %08X", got, ror(0x11223344, 8))
	}
	if got, _ := mem.Read32(0x2000); got != 0xAABBCCDD {
		t.Errorf("mem = %08X, want AABBCCDD", got)
	}
}

func TestSwpb(t *testing.T) {
	c, mem := newTestCore()
	if err := mem.Write32(0x2000, 0x11223344); err != nil {
		t.Fatal(err)
	}
	c.SetReg(1, 0x2002)
	c.SetReg(2, 0xFF)
	mustStep(t, c, mem, swpEnc(true, 1, 3, 2))
	if got := c.Reg(3); got != 0x22 {
		t.Errorf("SWPB: rd = %08X, want 22", got)
	}
	if got, _ := mem.Read32(0x2000); got != 0x11FF3344 {
		t.Errorf("SWPB: mem = %08X, want 11FF3344", got)
	}
}
