package arm

import "testing"

// thumbStep は hw を PC 位置に置いて Thumb state で 1 命令実行する。
func thumbStep(t *testing.T, c *Core, mem *testMem, hw uint16) error {
	t.Helper()
	if err := mem.Write16(c.PC(), hw); err != nil {
		t.Fatal(err)
	}
	return c.Step()
}

func mustThumbStep(t *testing.T, c *Core, mem *testMem, hw uint16) {
	t.Helper()
	if err := thumbStep(t, c, mem, hw); err != nil {
		t.Fatalf("Step(%04X): %v", hw, err)
	}
}

// newThumbCore は Thumb state のコアを作る。
func newThumbCore() (*Core, *testMem) {
	c, mem := newTestCore()
	c.SetCPSR(c.CPSR() | FlagT)
	return c, mem
}

func TestThumbShiftImm(t *testing.T) {
	tests := []struct {
		name      string
		hw        uint16
		rsVal     uint32
		want      uint32
		wantFlags string
	}{
		// LSL Rd, Rs, #imm: 000 00 imm5 rs rd
		{"LSL #4", 0x0111, 0x0F0F, 0xF0F0, ""},          // lsl r1, r2, #4
		{"LSL #0 (MOV)", 0x0011, 0x8000_0001, 0x80000001, "N"}, // lsl r1, r2, #0
		{"LSR #1 C", 0x0851, 0x3, 0x1, "C"},             // lsr r1, r2, #1
		{"LSR #0 = #32", 0x0811, 0x80000000, 0, "ZC"},   // lsr r1, r2, #32
		{"ASR #1", 0x1051, 0x80000002, 0xC0000001, "N"}, // asr r1, r2, #1
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			c, mem := newThumbCore()
			c.SetReg(2, tt.rsVal)
			mustThumbStep(t, c, mem, tt.hw)
			if got := c.Reg(1); got != tt.want {
				t.Errorf("r1 = %08X, want %08X", got, tt.want)
			}
			if got := flagsOf(c.CPSR()); got != tt.wantFlags {
				t.Errorf("flags = %q, want %q", got, tt.wantFlags)
			}
		})
	}
}

func TestThumbAddSub(t *testing.T) {
	c, mem := newThumbCore()
	c.SetReg(2, 100)
	c.SetReg(3, 42)
	mustThumbStep(t, c, mem, 0x18D1) // add r1, r2, r3
	if c.Reg(1) != 142 {
		t.Errorf("add: r1 = %d, want 142", c.Reg(1))
	}
	mustThumbStep(t, c, mem, 0x1AD1) // sub r1, r2, r3
	if c.Reg(1) != 58 || !c.CPSR().C() {
		t.Errorf("sub: r1 = %d C=%v, want 58 C=true", c.Reg(1), c.CPSR().C())
	}
	mustThumbStep(t, c, mem, 0x1DD1) // add r1, r2, #7
	if c.Reg(1) != 107 {
		t.Errorf("add imm3: r1 = %d, want 107", c.Reg(1))
	}
	mustThumbStep(t, c, mem, 0x1FD1) // sub r1, r2, #7
	if c.Reg(1) != 93 {
		t.Errorf("sub imm3: r1 = %d, want 93", c.Reg(1))
	}
}

func TestThumbImm8(t *testing.T) {
	c, mem := newThumbCore()
	mustThumbStep(t, c, mem, 0x21FF) // mov r1, #0xFF
	if c.Reg(1) != 0xFF {
		t.Errorf("mov: r1 = %X", c.Reg(1))
	}
	mustThumbStep(t, c, mem, 0x3105) // add r1, #5
	if c.Reg(1) != 0x104 {
		t.Errorf("add: r1 = %X, want 104", c.Reg(1))
	}
	mustThumbStep(t, c, mem, 0x3904) // sub r1, #4
	if c.Reg(1) != 0x100 {
		t.Errorf("sub: r1 = %X, want 100", c.Reg(1))
	}
}

func TestThumbCmpImm8(t *testing.T) {
	c, mem := newThumbCore()
	c.SetReg(1, 5)
	mustThumbStep(t, c, mem, 0x2905) // cmp r1, #5
	if got := flagsOf(c.CPSR()); got != "ZC" {
		t.Errorf("cmp equal: flags = %q, want ZC", got)
	}
	mustThumbStep(t, c, mem, 0x2906) // cmp r1, #6
	if got := flagsOf(c.CPSR()); got != "N" {
		t.Errorf("cmp less: flags = %q, want N", got)
	}
}

func TestThumbALU(t *testing.T) {
	tests := []struct {
		name      string
		op        uint16 // bits[9:6]
		rdVal     uint32
		rsVal     uint32
		want      uint32
		flagsIn   string
		wantFlags string
		testOnly  bool // TST/CMP/CMN は rd 不変
	}{
		{"AND", 0x0, 0xFF00FF, 0x00FFFF, 0x0000FF, "", "", false},
		{"EOR", 0x1, 0xFF, 0x0F, 0xF0, "", "", false},
		{"LSL reg", 0x2, 1, 8, 0x100, "", "", false},
		{"LSR reg", 0x3, 0x100, 8, 1, "", "", false},
		{"ASR reg", 0x4, 0x80000000, 31, 0xFFFFFFFF, "", "N", false}, // 最後に出るのは bit30=0 → C=0
		{"ADC C=1", 0x5, 10, 20, 31, "C", "", false},                 // キャリーアウトなし → C=0
		{"SBC C=0", 0x6, 10, 5, 4, "", "C", false},
		{"ROR", 0x7, 0xF000000F, 4, 0xFF000000, "", "NC", false},
		{"TST", 0x8, 0xF0, 0x0F, 0xF0, "", "Z", true},
		{"NEG", 0x9, 0, 5, 0xFFFFFFFB, "", "N", false},
		{"CMP eq", 0xA, 7, 7, 7, "", "ZC", true},
		{"CMN", 0xB, 1, 0xFFFFFFFF, 1, "", "ZC", true},
		{"ORR", 0xC, 0xF0, 0x0F, 0xFF, "", "", false},
		{"MUL", 0xD, 6, 7, 42, "", "", false},
		{"MUL C保存", 0xD, 6, 7, 42, "C", "C", false},
		{"BIC", 0xE, 0xFF, 0x0F, 0xF0, "", "", false},
		{"MVN", 0xF, 0, 0xFFFFFF00, 0xFF, "", "", false},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			c, mem := newThumbCore()
			setFlags(c, tt.flagsIn)
			c.SetReg(1, tt.rdVal)
			c.SetReg(2, tt.rsVal)
			mustThumbStep(t, c, mem, 0x4000|tt.op<<6|2<<3|1)
			wantRd := tt.want
			if tt.testOnly {
				wantRd = tt.rdVal
			}
			if got := c.Reg(1); got != wantRd {
				t.Errorf("r1 = %08X, want %08X", got, wantRd)
			}
			if got := flagsOf(c.CPSR()); got != tt.wantFlags {
				t.Errorf("flags = %q, want %q", got, tt.wantFlags)
			}
		})
	}
}

func TestThumbHiRegOps(t *testing.T) {
	c, mem := newThumbCore()
	c.SetReg(1, 10)
	c.SetReg(9, 32)
	mustThumbStep(t, c, mem, 0x4449) // add r1, r9
	if c.Reg(1) != 42 {
		t.Errorf("add hi: r1 = %d, want 42", c.Reg(1))
	}
	mustThumbStep(t, c, mem, 0x46C9) // mov r9, r9? → 0x46C9 = mov r9, r9
	mustThumbStep(t, c, mem, 0x468A) // mov r10, r1
	if c.Reg(10) != 42 {
		t.Errorf("mov hi: r10 = %d, want 42", c.Reg(10))
	}
	setFlags(c, "")
	mustThumbStep(t, c, mem, 0x4551) // cmp r1, r10
	if got := flagsOf(c.CPSR()); got != "ZC" {
		t.Errorf("cmp hi: flags = %q, want ZC", got)
	}
}

func TestThumbBX(t *testing.T) {
	// Thumb → ARM
	c, mem := newThumbCore()
	c.SetReg(3, 0x2000) // bit0=0 → ARM へ
	mustThumbStep(t, c, mem, 0x4718) // bx r3
	if c.CPSR().T() {
		t.Error("T should be cleared")
	}
	if c.PC() != 0x2000 {
		t.Errorf("PC = %08X, want 2000", c.PC())
	}
	// ARM 側で BX で Thumb に戻る（既存 ARM テストの逆方向）
	c.SetReg(4, 0x3001)
	mustStep(t, c, mem, 0xE12FFF14) // bx r4
	if !c.CPSR().T() || c.PC() != 0x3000 {
		t.Errorf("T=%v PC=%08X, want T=true PC=3000", c.CPSR().T(), c.PC())
	}
}

func TestThumbPCRelativeLoad(t *testing.T) {
	c, mem := newThumbCore()
	// PC=0x1000。ベース = (0x1000+4)&^3 = 0x1004。imm=4 → 0x1014
	if err := mem.Write32(0x1014, 0xCAFEF00D); err != nil {
		t.Fatal(err)
	}
	mustThumbStep(t, c, mem, 0x4904) // ldr r1, [pc, #16]
	if c.Reg(1) != 0xCAFEF00D {
		t.Errorf("r1 = %08X", c.Reg(1))
	}
}

func TestThumbLoadStore(t *testing.T) {
	c, mem := newThumbCore()
	c.SetReg(1, 0x2000)
	c.SetReg(2, 4)
	c.SetReg(3, 0xAABBCCDD)

	mustThumbStep(t, c, mem, 0x508B) // str r3, [r1, r2]
	if v, _ := mem.Read32(0x2004); v != 0xAABBCCDD {
		t.Errorf("str reg: %08X", v)
	}
	mustThumbStep(t, c, mem, 0x588C) // ldr r4, [r1, r2]
	if c.Reg(4) != 0xAABBCCDD {
		t.Errorf("ldr reg: %08X", c.Reg(4))
	}
	mustThumbStep(t, c, mem, 0x684D) // ldr r5, [r1, #4]
	if c.Reg(5) != 0xAABBCCDD {
		t.Errorf("ldr imm: %08X", c.Reg(5))
	}
	mustThumbStep(t, c, mem, 0x708B) // strb r3, [r1, #2]
	if v, _ := mem.Read8(0x2002); v != 0xDD {
		t.Errorf("strb: %02X", v)
	}
	mustThumbStep(t, c, mem, 0x788C) // ldrb r4, [r1, #2]
	if c.Reg(4) != 0xDD {
		t.Errorf("ldrb: %08X", c.Reg(4))
	}
	mustThumbStep(t, c, mem, 0x800B) // strh r3, [r1, #0]
	if v, _ := mem.Read16(0x2000); v != 0xCCDD {
		t.Errorf("strh: %04X", v)
	}
	mustThumbStep(t, c, mem, 0x880C) // ldrh r4, [r1, #0]
	if c.Reg(4) != 0xCCDD {
		t.Errorf("ldrh: %08X", c.Reg(4))
	}
	// 符号付きロード（[r1+r2] = 0x2004 には 0xAABBCCDD が入っている）
	mustThumbStep(t, c, mem, 0x568C) // ldrsb r4, [r1, r2]
	if c.Reg(4) != 0xFFFFFFDD {
		t.Errorf("ldrsb: %08X, want FFFFFFDD", c.Reg(4))
	}
	mustThumbStep(t, c, mem, 0x5E8C) // ldrsh r4, [r1, r2]
	if c.Reg(4) != 0xFFFFCCDD {
		t.Errorf("ldrsh: %08X, want FFFFCCDD", c.Reg(4))
	}
}

func TestThumbSPRelative(t *testing.T) {
	c, mem := newThumbCore()
	c.SetReg(13, 0x4000)
	c.SetReg(1, 0x12345678)
	mustThumbStep(t, c, mem, 0x9102) // str r1, [sp, #8]
	if v, _ := mem.Read32(0x4008); v != 0x12345678 {
		t.Errorf("str sp: %08X", v)
	}
	mustThumbStep(t, c, mem, 0x9A02) // ldr r2, [sp, #8]
	if c.Reg(2) != 0x12345678 {
		t.Errorf("ldr sp: %08X", c.Reg(2))
	}
	mustThumbStep(t, c, mem, 0xA903) // add r1, sp, #12
	if c.Reg(1) != 0x400C {
		t.Errorf("add sp rel: %08X", c.Reg(1))
	}
	mustThumbStep(t, c, mem, 0xA201) // add r2, pc, #4 → (PC+4)&^3 + 4
	if want := (c.PC()-2+4)&^uint32(3) + 4; c.Reg(2) != want {
		t.Errorf("add pc rel: %08X, want %08X", c.Reg(2), want)
	}
	mustThumbStep(t, c, mem, 0xB082) // sub sp, #8
	if c.Reg(13) != 0x3FF8 {
		t.Errorf("sub sp: %08X", c.Reg(13))
	}
	mustThumbStep(t, c, mem, 0xB002) // add sp, #8
	if c.Reg(13) != 0x4000 {
		t.Errorf("add sp: %08X", c.Reg(13))
	}
}

func TestThumbPushPop(t *testing.T) {
	c, mem := newThumbCore()
	c.SetReg(13, 0x4000)
	c.SetReg(1, 0x11)
	c.SetReg(2, 0x22)
	c.SetReg(14, 0x1235) // LR（Thumb の戻り先 | 1）

	mustThumbStep(t, c, mem, 0xB506) // push {r1, r2, lr}
	if c.Reg(13) != 0x4000-12 {
		t.Fatalf("sp after push = %08X", c.Reg(13))
	}
	for i, want := range []uint32{0x11, 0x22, 0x1235} {
		if v, _ := mem.Read32(0x4000 - 12 + uint32(i)*4); v != want {
			t.Errorf("stack[%d] = %08X, want %08X", i, v, want)
		}
	}

	c.SetReg(1, 0)
	c.SetReg(2, 0)
	mustThumbStep(t, c, mem, 0xBD06) // pop {r1, r2, pc}
	if c.Reg(1) != 0x11 || c.Reg(2) != 0x22 {
		t.Errorf("pop regs: r1=%X r2=%X", c.Reg(1), c.Reg(2))
	}
	if c.Reg(13) != 0x4000 {
		t.Errorf("sp after pop = %08X", c.Reg(13))
	}
	// v4T: POP {pc} は状態を変えない（bit0 は無視）
	if c.PC() != 0x1234 || !c.CPSR().T() {
		t.Errorf("PC = %08X T=%v, want 1234/true", c.PC(), c.CPSR().T())
	}
}

func TestThumbLdmStm(t *testing.T) {
	c, mem := newThumbCore()
	c.SetReg(0, 0x3000)
	c.SetReg(1, 0xAA)
	c.SetReg(2, 0xBB)
	mustThumbStep(t, c, mem, 0xC006) // stmia r0!, {r1, r2}
	if c.Reg(0) != 0x3008 {
		t.Errorf("stm writeback: %08X", c.Reg(0))
	}
	if v, _ := mem.Read32(0x3000); v != 0xAA {
		t.Errorf("stm[0] = %X", v)
	}
	c.SetReg(0, 0x3000)
	c.SetReg(1, 0)
	c.SetReg(2, 0)
	mustThumbStep(t, c, mem, 0xC806) // ldmia r0!, {r1, r2}
	if c.Reg(1) != 0xAA || c.Reg(2) != 0xBB || c.Reg(0) != 0x3008 {
		t.Errorf("ldm: r1=%X r2=%X r0=%X", c.Reg(1), c.Reg(2), c.Reg(0))
	}
}

func TestThumbCondBranch(t *testing.T) {
	c, mem := newThumbCore()
	setFlags(c, "Z")
	mustThumbStep(t, c, mem, 0xD003) // beq +6（PC+4+6 = 0x100A... imm=3 → +6）
	if want := uint32(testPC + 4 + 6); c.PC() != want {
		t.Errorf("beq taken: PC = %08X, want %08X", c.PC(), want)
	}
	// 不成立なら次の命令へ
	c2, mem2 := newThumbCore()
	setFlags(c2, "")
	mustThumbStep(t, c2, mem2, 0xD003) // beq（Z=0 → 不成立）
	if want := uint32(testPC + 2); c2.PC() != want {
		t.Errorf("beq not taken: PC = %08X, want %08X", c2.PC(), want)
	}
	// 後方分岐
	c3, mem3 := newThumbCore()
	setFlags(c3, "")
	mustThumbStep(t, c3, mem3, 0xD1FE) // bne -4 → PC+4-4 = PC
	if want := uint32(testPC); c3.PC() != want {
		t.Errorf("bne back: PC = %08X, want %08X", c3.PC(), want)
	}
}

func TestThumbUncondBranch(t *testing.T) {
	c, mem := newThumbCore()
	mustThumbStep(t, c, mem, 0xE010) // b +32
	if want := uint32(testPC + 4 + 32); c.PC() != want {
		t.Errorf("b: PC = %08X, want %08X", c.PC(), want)
	}
}

func TestThumbBL(t *testing.T) {
	c, mem := newThumbCore()
	// BL +0x100: prefix(imm11=0) + suffix(imm11=0x80)
	mustThumbStep(t, c, mem, 0xF000) // bl prefix: lr = PC+4 + 0
	mustThumbStep(t, c, mem, 0xF880) // bl suffix: pc = lr + 0x100
	if want := uint32(testPC + 4 + 0x100); c.PC() != want {
		t.Errorf("bl: PC = %08X, want %08X", c.PC(), want)
	}
	// LR = suffix の次の命令 | 1
	if want := uint32(testPC+4) | 1; c.Reg(14) != want {
		t.Errorf("bl: LR = %08X, want %08X", c.Reg(14), want)
	}
	// 負のオフセット
	c2, mem2 := newThumbCore()
	mustThumbStep(t, c2, mem2, 0xF7FF) // prefix: lr = PC+4 + (-1<<12)
	mustThumbStep(t, c2, mem2, 0xFFFE) // suffix: pc = lr + 0x7FC... → PC+4 - 0x1000 + 0xFFC = PC
	if want := uint32(testPC + 4 - 4); c2.PC() != want {
		t.Errorf("bl neg: PC = %08X, want %08X", c2.PC(), want)
	}
}

func TestThumbSWI(t *testing.T) {
	c, mem := newThumbCore()
	mustThumbStep(t, c, mem, 0xDF10) // swi #0x10
	if c.cpsr.Mode() != ModeSvc || c.cpsr.T() {
		t.Errorf("mode=%02X T=%v, want SVC/false", c.cpsr.Mode(), c.cpsr.T())
	}
	if c.PC() != VecSWI {
		t.Errorf("PC = %08X, want %08X", c.PC(), uint32(VecSWI))
	}
	// LR = SWI の次の Thumb 命令
	if want := uint32(testPC + 2); c.Reg(14) != want {
		t.Errorf("LR = %08X, want %08X", c.Reg(14), want)
	}
	// SPSR の T が立っているので MOVS pc, lr で Thumb に復帰できる
	if !c.SPSR().T() {
		t.Error("SPSR.T should be set")
	}
}
