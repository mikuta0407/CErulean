package arm

import (
	"errors"
	"fmt"
	"testing"
)

// ---- テスト用メモリ（bus パッケージに依存しないための最小実装）----

type testMem struct{ data []byte }

func (m *testMem) check(addr uint32, size int) error {
	if int(addr)+size > len(m.data) {
		return fmt.Errorf("testMem: out of range access at %08X", addr)
	}
	return nil
}
func (m *testMem) Read8(a uint32) (uint8, error) {
	if err := m.check(a, 1); err != nil {
		return 0, err
	}
	return m.data[a], nil
}
func (m *testMem) Read16(a uint32) (uint16, error) {
	if err := m.check(a, 2); err != nil {
		return 0, err
	}
	return uint16(m.data[a]) | uint16(m.data[a+1])<<8, nil
}
func (m *testMem) Read32(a uint32) (uint32, error) {
	if err := m.check(a, 4); err != nil {
		return 0, err
	}
	return uint32(m.data[a]) | uint32(m.data[a+1])<<8 | uint32(m.data[a+2])<<16 | uint32(m.data[a+3])<<24, nil
}
func (m *testMem) Write8(a uint32, v uint8) error {
	if err := m.check(a, 1); err != nil {
		return err
	}
	m.data[a] = v
	return nil
}
func (m *testMem) Write16(a uint32, v uint16) error {
	if err := m.check(a, 2); err != nil {
		return err
	}
	m.data[a], m.data[a+1] = uint8(v), uint8(v>>8)
	return nil
}
func (m *testMem) Write32(a uint32, v uint32) error {
	if err := m.check(a, 4); err != nil {
		return err
	}
	m.data[a], m.data[a+1], m.data[a+2], m.data[a+3] = uint8(v), uint8(v>>8), uint8(v>>16), uint8(v>>24)
	return nil
}

const testPC = 0x1000

// newTestCore は 64KB RAM（アドレス 0 起点）を持つコアを作り、PC を testPC にする。
func newTestCore() (*Core, *testMem) {
	mem := &testMem{data: make([]byte, 64*1024)}
	c := New(mem, nil)
	c.Reset(testPC)
	return c, mem
}

// stepOne は word を PC 位置に置いて 1 命令実行する。
func stepOne(t *testing.T, c *Core, mem *testMem, word uint32) error {
	t.Helper()
	if err := mem.Write32(c.PC(), word); err != nil {
		t.Fatal(err)
	}
	return c.Step()
}

func mustStep(t *testing.T, c *Core, mem *testMem, word uint32) {
	t.Helper()
	if err := stepOne(t, c, mem, word); err != nil {
		t.Fatalf("Step(%08X): %v", word, err)
	}
}

// ---- 命令エンコードヘルパー（テストの可読性のため）----

const condAL = 0xE

// dpImm: データ処理・即値形式。imm を rot*2 右ローテートした値がオペランド。
func dpImm(op uint32, s bool, rn, rd, rot, imm uint32) uint32 {
	w := condAL<<28 | 1<<25 | op<<21 | rn<<16 | rd<<12 | rot<<8 | imm
	if s {
		w |= 1 << 20
	}
	return w
}

// dpReg: データ処理・レジスタ形式（即値シフト量）。
func dpReg(op uint32, s bool, rn, rd, rm, shiftType, amount uint32) uint32 {
	w := condAL<<28 | op<<21 | rn<<16 | rd<<12 | amount<<7 | shiftType<<5 | rm
	if s {
		w |= 1 << 20
	}
	return w
}

// dpRegShift: データ処理・レジスタ指定シフト量。
func dpRegShift(op uint32, s bool, rn, rd, rm, shiftType, rs uint32) uint32 {
	w := condAL<<28 | op<<21 | rn<<16 | rd<<12 | rs<<8 | shiftType<<5 | 1<<4 | rm
	if s {
		w |= 1 << 20
	}
	return w
}

// flagsOf は PSR のフラグを "NZCV" の部分文字列で表す（テスト比較用）。
func flagsOf(p PSR) string {
	s := ""
	if p.N() {
		s += "N"
	}
	if p.Z() {
		s += "Z"
	}
	if p.C() {
		s += "C"
	}
	if p.V() {
		s += "V"
	}
	return s
}

// setFlags は CPSR の NZCV を文字列指定で設定する。
func setFlags(c *Core, flags string) {
	p := c.cpsr &^ (FlagN | FlagZ | FlagC | FlagV)
	for _, f := range flags {
		switch f {
		case 'N':
			p |= FlagN
		case 'Z':
			p |= FlagZ
		case 'C':
			p |= FlagC
		case 'V':
			p |= FlagV
		}
	}
	c.cpsr = p
}

// ---- データ処理: フラグ計算 ----

func TestDataProcFlags(t *testing.T) {
	tests := []struct {
		name    string
		word    uint32
		r1, r2  uint32 // 入力: r1 = Rn, r2 = Rm/Rs 用
		inFlags string
		wantRd  uint32 // rd=r0 の期待値（テスト命令 CMP 等では無視）
		wantFl  string
		noRd    bool // CMP/TST 系: r0 が書かれないことを確認
	}{
		// ADD/ADDS
		{name: "ADDS zero", word: dpReg(opADD, true, 1, 0, 2, 0, 0), r1: 0, r2: 0, wantRd: 0, wantFl: "Z"},
		{name: "ADDS overflow pos", word: dpReg(opADD, true, 1, 0, 2, 0, 0), r1: 0x7FFFFFFF, r2: 1, wantRd: 0x80000000, wantFl: "NV"},
		{name: "ADDS carry wrap", word: dpReg(opADD, true, 1, 0, 2, 0, 0), r1: 0xFFFFFFFF, r2: 1, wantRd: 0, wantFl: "ZC"},
		{name: "ADDS neg+neg overflow", word: dpReg(opADD, true, 1, 0, 2, 0, 0), r1: 0x80000000, r2: 0x80000000, wantRd: 0, wantFl: "ZCV"},
		{name: "ADDS plain", word: dpImm(opADD, true, 1, 0, 0, 3), r1: 4, wantRd: 7, wantFl: ""},
		// ADC: キャリー入力
		{name: "ADCS with carry", word: dpReg(opADC, true, 1, 0, 2, 0, 0), r1: 0xFFFFFFFF, r2: 0, inFlags: "C", wantRd: 0, wantFl: "ZC"},
		{name: "ADCS no carry", word: dpReg(opADC, true, 1, 0, 2, 0, 0), r1: 0xFFFFFFFF, r2: 0, wantRd: 0xFFFFFFFF, wantFl: "N"},
		// SUB: C は「ボローなし」
		{name: "SUBS no borrow", word: dpImm(opSUB, true, 1, 0, 0, 3), r1: 5, wantRd: 2, wantFl: "C"},
		{name: "SUBS borrow", word: dpImm(opSUB, true, 1, 0, 0, 5), r1: 3, wantRd: 0xFFFFFFFE, wantFl: "N"},
		{name: "SUBS equal", word: dpImm(opSUB, true, 1, 0, 0, 7), r1: 7, wantRd: 0, wantFl: "ZC"},
		{name: "SUBS overflow", word: dpImm(opSUB, true, 1, 0, 0, 1), r1: 0x80000000, wantRd: 0x7FFFFFFF, wantFl: "CV"},
		// SBC: C=0 なら追加で 1 引く
		{name: "SBCS carry set", word: dpImm(opSBC, true, 1, 0, 0, 3), r1: 5, inFlags: "C", wantRd: 2, wantFl: "C"},
		{name: "SBCS carry clear", word: dpImm(opSBC, true, 1, 0, 0, 3), r1: 5, wantRd: 1, wantFl: "C"},
		// RSB/RSC
		{name: "RSBS", word: dpImm(opRSB, true, 1, 0, 0, 10), r1: 3, wantRd: 7, wantFl: "C"},
		{name: "RSCS carry clear", word: dpImm(opRSC, true, 1, 0, 0, 10), r1: 3, wantRd: 6, wantFl: "C"},
		// 比較・テスト命令（rd は書かれない）
		{name: "CMP equal", word: dpImm(opCMP, true, 1, 0, 0, 9), r1: 9, wantFl: "ZC", noRd: true},
		{name: "CMP less", word: dpImm(opCMP, true, 1, 0, 0, 9), r1: 5, wantFl: "N", noRd: true},
		{name: "CMN", word: dpImm(opCMN, true, 1, 0, 0, 1), r1: 0xFFFFFFFF, wantFl: "ZC", noRd: true},
		{name: "TST zero", word: dpImm(opTST, true, 1, 0, 0, 0xF0), r1: 0x0F, wantFl: "Z", noRd: true},
		{name: "TEQ same", word: dpReg(opTEQ, true, 1, 0, 2, 0, 0), r1: 0xAA55, r2: 0xAA55, wantFl: "Z", noRd: true},
		// 論理系: C はシフタキャリー、V は不変
		{name: "ANDS keeps V", word: dpImm(opAND, true, 1, 0, 0, 0xFF), r1: 0x80000001, inFlags: "V", wantRd: 1, wantFl: "V"},
		{name: "ORRS negative", word: dpImm(opORR, true, 1, 0, 0, 0), r1: 0x80000000, wantRd: 0x80000000, wantFl: "N"},
		{name: "EORS", word: dpReg(opEOR, true, 1, 0, 2, 0, 0), r1: 0xFF00, r2: 0x0FF0, wantRd: 0xF0F0, wantFl: ""},
		{name: "BICS", word: dpImm(opBIC, true, 1, 0, 0, 0x0F), r1: 0xFF, wantRd: 0xF0, wantFl: ""},
		{name: "MOVS zero", word: dpImm(opMOV, true, 0, 0, 0, 0), wantRd: 0, wantFl: "Z"},
		{name: "MVNS", word: dpImm(opMVN, true, 0, 0, 0, 0), wantRd: 0xFFFFFFFF, wantFl: "N"},
		// 即値ローテートのシフタキャリー: rot!=0 なら C = 結果の bit31
		{name: "MOVS imm rot carry", word: dpImm(opMOV, true, 0, 0, 2, 0xFF), wantRd: 0xF000000F, wantFl: "NC"},
		{name: "MOVS imm rot0 keeps C", word: dpImm(opMOV, true, 0, 0, 0, 1), inFlags: "C", wantRd: 1, wantFl: "C"},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			c, mem := newTestCore()
			c.regs[0] = 0xDEADBEEF
			c.regs[1] = tt.r1
			c.regs[2] = tt.r2
			setFlags(c, tt.inFlags)
			mustStep(t, c, mem, tt.word)
			if tt.noRd {
				if c.regs[0] != 0xDEADBEEF {
					t.Errorf("r0 was written: %08X", c.regs[0])
				}
			} else if c.regs[0] != tt.wantRd {
				t.Errorf("r0 = %08X, want %08X", c.regs[0], tt.wantRd)
			}
			if got := flagsOf(c.cpsr); got != tt.wantFl {
				t.Errorf("flags = %q, want %q", got, tt.wantFl)
			}
		})
	}
}

// ---- バレルシフタ ----

func TestShifterCarry(t *testing.T) {
	// MOVS r0, r1, <shift> の形で、結果とシフタキャリーを網羅する。
	tests := []struct {
		name    string
		word    uint32
		r1, r2  uint32 // r2 はレジスタ指定シフト量
		inFlags string
		want    uint32
		wantFl  string
	}{
		// 即値シフト
		{name: "LSL #0 keeps C", word: dpReg(opMOV, true, 0, 0, 1, 0, 0), r1: 2, inFlags: "C", want: 2, wantFl: "C"},
		{name: "LSL #1 carry out", word: dpReg(opMOV, true, 0, 0, 1, 0, 1), r1: 0x80000001, want: 2, wantFl: "C"},
		{name: "LSL #4 no carry", word: dpReg(opMOV, true, 0, 0, 1, 0, 4), r1: 0x0F000001, want: 0xF0000010, wantFl: "N"},
		{name: "LSR #1", word: dpReg(opMOV, true, 0, 0, 1, 1, 1), r1: 3, want: 1, wantFl: "C"},
		{name: "LSR #32 (enc 0)", word: dpReg(opMOV, true, 0, 0, 1, 1, 0), r1: 0x80000000, want: 0, wantFl: "ZC"},
		{name: "ASR #1", word: dpReg(opMOV, true, 0, 0, 1, 2, 1), r1: 0x80000001, want: 0xC0000000, wantFl: "NC"},
		{name: "ASR #32 (enc 0) neg", word: dpReg(opMOV, true, 0, 0, 1, 2, 0), r1: 0x80000000, want: 0xFFFFFFFF, wantFl: "NC"},
		{name: "ASR #32 (enc 0) pos", word: dpReg(opMOV, true, 0, 0, 1, 2, 0), r1: 0x7FFFFFFF, want: 0, wantFl: "Z"},
		{name: "ROR #8", word: dpReg(opMOV, true, 0, 0, 1, 3, 8), r1: 0x000000FF, want: 0xFF000000, wantFl: "NC"},
		{name: "RRX (ROR #0) C in", word: dpReg(opMOV, true, 0, 0, 1, 3, 0), r1: 2, inFlags: "C", want: 0x80000001, wantFl: "N"},
		{name: "RRX C out", word: dpReg(opMOV, true, 0, 0, 1, 3, 0), r1: 1, want: 0, wantFl: "ZC"},
		// レジスタ指定シフト
		{name: "LSL reg 0 keeps C", word: dpRegShift(opMOV, true, 0, 0, 1, 0, 2), r1: 5, r2: 0, inFlags: "C", want: 5, wantFl: "C"},
		{name: "LSL reg 32", word: dpRegShift(opMOV, true, 0, 0, 1, 0, 2), r1: 1, r2: 32, want: 0, wantFl: "ZC"},
		{name: "LSL reg 33", word: dpRegShift(opMOV, true, 0, 0, 1, 0, 2), r1: 0xFFFFFFFF, r2: 33, want: 0, wantFl: "Z"},
		{name: "LSL reg uses low byte", word: dpRegShift(opMOV, true, 0, 0, 1, 0, 2), r1: 1, r2: 0x100, want: 1, wantFl: ""},
		{name: "LSR reg 32", word: dpRegShift(opMOV, true, 0, 0, 1, 1, 2), r1: 0x80000000, r2: 32, want: 0, wantFl: "ZC"},
		{name: "LSR reg 40", word: dpRegShift(opMOV, true, 0, 0, 1, 1, 2), r1: 0xFFFFFFFF, r2: 40, want: 0, wantFl: "Z"},
		{name: "ASR reg 40 neg", word: dpRegShift(opMOV, true, 0, 0, 1, 2, 2), r1: 0x80000000, r2: 40, want: 0xFFFFFFFF, wantFl: "NC"},
		{name: "ROR reg 32 (C=bit31)", word: dpRegShift(opMOV, true, 0, 0, 1, 3, 2), r1: 0x80000001, r2: 32, want: 0x80000001, wantFl: "NC"},
		{name: "ROR reg 4", word: dpRegShift(opMOV, true, 0, 0, 1, 3, 2), r1: 0x0000000F, r2: 4, want: 0xF0000000, wantFl: "NC"},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			c, mem := newTestCore()
			c.regs[1] = tt.r1
			c.regs[2] = tt.r2
			setFlags(c, tt.inFlags)
			mustStep(t, c, mem, tt.word)
			if c.regs[0] != tt.want {
				t.Errorf("r0 = %08X, want %08X", c.regs[0], tt.want)
			}
			if got := flagsOf(c.cpsr); got != tt.wantFl {
				t.Errorf("flags = %q, want %q", got, tt.wantFl)
			}
		})
	}
}

// ---- 条件実行 ----

func TestConditionCodes(t *testing.T) {
	// <cond> MOV r0, #1 を各フラグ状態で実行し、成立/不成立を確認する。
	tests := []struct {
		cond  uint32
		flags string
		exec  bool
	}{
		{0x0, "Z", true}, {0x0, "", false}, // EQ
		{0x1, "", true}, {0x1, "Z", false}, // NE
		{0x2, "C", true}, {0x2, "", false}, // CS
		{0x3, "", true}, {0x3, "C", false}, // CC
		{0x4, "N", true}, {0x4, "", false}, // MI
		{0x5, "", true}, {0x5, "N", false}, // PL
		{0x6, "V", true}, {0x6, "", false}, // VS
		{0x7, "", true}, {0x7, "V", false}, // VC
		{0x8, "C", true}, {0x8, "CZ", false}, {0x8, "", false}, // HI
		{0x9, "Z", true}, {0x9, "", true}, {0x9, "C", false}, // LS
		{0xA, "NV", true}, {0xA, "", true}, {0xA, "N", false}, // GE
		{0xB, "N", true}, {0xB, "NV", false}, // LT
		{0xC, "NV", true}, {0xC, "ZNV", false}, {0xC, "N", false}, // GT
		{0xD, "Z", true}, {0xD, "N", true}, {0xD, "NV", false}, // LE
		{0xE, "", true}, // AL
	}
	for _, tt := range tests {
		t.Run(fmt.Sprintf("cond%X_flags%q", tt.cond, tt.flags), func(t *testing.T) {
			c, mem := newTestCore()
			setFlags(c, tt.flags)
			word := tt.cond<<28 | 1<<25 | uint32(opMOV)<<21 | 0<<12 | 1 // MOV r0, #1
			mustStep(t, c, mem, word)
			want := uint32(0)
			if tt.exec {
				want = 1
			}
			if c.regs[0] != want {
				t.Errorf("r0 = %d, want %d", c.regs[0], want)
			}
			if c.PC() != testPC+4 {
				t.Errorf("PC = %08X, want %08X", c.PC(), uint32(testPC+4))
			}
		})
	}
}

// ---- PC（r15）の見え方 ----

func TestPCOperand(t *testing.T) {
	c, mem := newTestCore()
	// MOV r0, pc → PC+8
	mustStep(t, c, mem, dpReg(opMOV, false, 0, 0, 15, 0, 0))
	if want := uint32(testPC + 8); c.regs[0] != want {
		t.Errorf("MOV r0, pc: r0 = %08X, want %08X", c.regs[0], want)
	}
	// ADD r0, pc, #4 → PC+8+4
	c.Reset(testPC)
	mustStep(t, c, mem, dpImm(opADD, false, 15, 0, 0, 4))
	if want := uint32(testPC + 12); c.regs[0] != want {
		t.Errorf("ADD r0, pc, #4: r0 = %08X, want %08X", c.regs[0], want)
	}
	// MOV pc, r1 → 分岐（bit1:0 は無視される）
	c.Reset(testPC)
	c.regs[1] = 0x2003
	mustStep(t, c, mem, dpReg(opMOV, false, 0, 15, 1, 0, 0))
	if c.PC() != 0x2000 {
		t.Errorf("MOV pc, r1: PC = %08X, want 00002000", c.PC())
	}
}

// ---- 分岐 ----

func TestBranch(t *testing.T) {
	c, mem := newTestCore()
	// B +8 (offset フィールド = 2): 飛び先 = PC+8+8
	mustStep(t, c, mem, 0xEA000000|2)
	if want := uint32(testPC + 16); c.PC() != want {
		t.Errorf("B: PC = %08X, want %08X", c.PC(), want)
	}

	// B 後方: offset = -4 命令 (0xFFFFFC)
	c.Reset(testPC)
	mustStep(t, c, mem, 0xEA000000|0x00FFFFFC)
	if want := uint32(testPC + 8 - 16); c.PC() != want {
		t.Errorf("B backward: PC = %08X, want %08X", c.PC(), want)
	}

	// BL: lr = 次の命令
	c.Reset(testPC)
	mustStep(t, c, mem, 0xEB000000|2)
	if want := uint32(testPC + 4); c.regs[14] != want {
		t.Errorf("BL: lr = %08X, want %08X", c.regs[14], want)
	}
	if want := uint32(testPC + 16); c.PC() != want {
		t.Errorf("BL: PC = %08X, want %08X", c.PC(), want)
	}
}

func TestBX(t *testing.T) {
	// ARM のまま分岐
	c, mem := newTestCore()
	c.regs[3] = 0x3000
	mustStep(t, c, mem, 0xE12FFF10|3)
	if c.PC() != 0x3000 || c.cpsr.T() {
		t.Errorf("BX (ARM): PC=%08X T=%v", c.PC(), c.cpsr.T())
	}

	// Thumb へ切り替え → 次の Step は未実装エラーで停止
	c.Reset(testPC)
	c.regs[3] = 0x3001
	mustStep(t, c, mem, 0xE12FFF10|3)
	if c.PC() != 0x3000 || !c.cpsr.T() {
		t.Errorf("BX (Thumb): PC=%08X T=%v", c.PC(), c.cpsr.T())
	}
	err := c.Step()
	var ue *UndefinedError
	if !errors.As(err, &ue) {
		t.Fatalf("Step in Thumb state: err = %v, want UndefinedError", err)
	}
}

// ---- ロード/ストア ----

func TestLoadStoreWord(t *testing.T) {
	c, mem := newTestCore()
	c.regs[1] = 0x2000
	c.regs[2] = 0x11223344

	// STR r2, [r1, #4]
	mustStep(t, c, mem, 0xE5812004)
	if v, _ := mem.Read32(0x2004); v != 0x11223344 {
		t.Errorf("STR: mem[2004] = %08X", v)
	}
	// LDR r0, [r1, #4]
	mustStep(t, c, mem, 0xE5910004)
	if c.regs[0] != 0x11223344 {
		t.Errorf("LDR: r0 = %08X", c.regs[0])
	}
	// プリインデックス+ライトバック: LDR r0, [r1, #4]!
	c.regs[1] = 0x2000
	mustStep(t, c, mem, 0xE5B10004)
	if c.regs[0] != 0x11223344 || c.regs[1] != 0x2004 {
		t.Errorf("LDR pre+wb: r0=%08X r1=%08X", c.regs[0], c.regs[1])
	}
	// ポストインデックス: LDR r0, [r1], #4（アクセスは旧ベース、r1 は +4）
	c.regs[1] = 0x2004
	c.regs[0] = 0
	mustStep(t, c, mem, 0xE4910004)
	if c.regs[0] != 0x11223344 || c.regs[1] != 0x2008 {
		t.Errorf("LDR post: r0=%08X r1=%08X", c.regs[0], c.regs[1])
	}
	// 減算オフセット: LDR r0, [r1, #-4]（r1=0x2008）
	c.regs[0] = 0
	mustStep(t, c, mem, 0xE5110004)
	if c.regs[0] != 0x11223344 {
		t.Errorf("LDR neg offset: r0=%08X", c.regs[0])
	}
	// レジスタオフセット（スケーリング付き）: LDR r0, [r1, r3, LSL #2]
	c.regs[1] = 0x2000
	c.regs[3] = 1
	c.regs[0] = 0
	mustStep(t, c, mem, 0xE7910103)
	if c.regs[0] != 0x11223344 {
		t.Errorf("LDR reg offset: r0=%08X", c.regs[0])
	}
}

func TestLoadStoreByteHalf(t *testing.T) {
	c, mem := newTestCore()
	c.regs[1] = 0x2000
	c.regs[2] = 0x11223344
	// STRB r2, [r1]
	mustStep(t, c, mem, 0xE5C12000)
	if v, _ := mem.Read8(0x2000); v != 0x44 {
		t.Errorf("STRB: mem[2000] = %02X", v)
	}
	// LDRB r0, [r1]
	mustStep(t, c, mem, 0xE5D10000)
	if c.regs[0] != 0x44 {
		t.Errorf("LDRB: r0 = %08X", c.regs[0])
	}
	// STRH r2, [r1, #2]（即値形式: 0xE1C120B2）
	mustStep(t, c, mem, 0xE1C120B2)
	if v, _ := mem.Read16(0x2002); v != 0x3344 {
		t.Errorf("STRH: mem[2002] = %04X", v)
	}
	// LDRH r0, [r1, #2]
	mustStep(t, c, mem, 0xE1D100B2)
	if c.regs[0] != 0x3344 {
		t.Errorf("LDRH: r0 = %08X", c.regs[0])
	}
	// LDRSB r0, [r1]（0x44 → 正: そのまま）と負値
	mustStep(t, c, mem, 0xE1D100D0)
	if c.regs[0] != 0x44 {
		t.Errorf("LDRSB pos: r0 = %08X", c.regs[0])
	}
	_ = mem.Write8(0x2000, 0x80)
	mustStep(t, c, mem, 0xE1D100D0)
	if c.regs[0] != 0xFFFFFF80 {
		t.Errorf("LDRSB neg: r0 = %08X", c.regs[0])
	}
	// LDRSH r0, [r1, #2]（0x3344 → 正）
	mustStep(t, c, mem, 0xE1D100F2)
	if c.regs[0] != 0x3344 {
		t.Errorf("LDRSH: r0 = %08X", c.regs[0])
	}
}

func TestLoadUnalignedRotate(t *testing.T) {
	// ARMv4 の非アラインワードロードはロートされた値になる。
	c, mem := newTestCore()
	_ = mem.Write32(0x2000, 0x11223344)
	c.regs[1] = 0x2001
	mustStep(t, c, mem, 0xE5910000) // LDR r0, [r1]
	if c.regs[0] != 0x44112233 {
		t.Errorf("unaligned LDR: r0 = %08X, want 44112233", c.regs[0])
	}
}

func TestLdrSameRegWins(t *testing.T) {
	// LDR r1, [r1], #4: ロード値がライトバックに勝つ
	c, mem := newTestCore()
	_ = mem.Write32(0x2000, 0xCAFEBABE)
	c.regs[1] = 0x2000
	mustStep(t, c, mem, 0xE4911004)
	if c.regs[1] != 0xCAFEBABE {
		t.Errorf("LDR r1,[r1],#4: r1 = %08X, want CAFEBABE", c.regs[1])
	}
}

// ---- LDM/STM ----

func TestLdmStm(t *testing.T) {
	c, mem := newTestCore()
	c.regs[0], c.regs[1], c.regs[2] = 0xAAAA0000, 0xBBBB1111, 0xCCCC2222

	// STMDB r13!, {r0-r2}（プッシュ）
	c.regs[13] = 0x3000
	mustStep(t, c, mem, 0xE92D0007)
	if c.regs[13] != 0x3000-12 {
		t.Errorf("STMDB: sp = %08X", c.regs[13])
	}
	for i, want := range []uint32{0xAAAA0000, 0xBBBB1111, 0xCCCC2222} {
		if v, _ := mem.Read32(uint32(0x2FF4 + 4*i)); v != want {
			t.Errorf("STMDB: mem[%08X] = %08X, want %08X", 0x2FF4+4*i, v, want)
		}
	}

	// LDMIA r13!, {r3-r5}（ポップ）
	mustStep(t, c, mem, 0xE8BD0038)
	if c.regs[3] != 0xAAAA0000 || c.regs[4] != 0xBBBB1111 || c.regs[5] != 0xCCCC2222 {
		t.Errorf("LDMIA: r3-r5 = %08X %08X %08X", c.regs[3], c.regs[4], c.regs[5])
	}
	if c.regs[13] != 0x3000 {
		t.Errorf("LDMIA: sp = %08X", c.regs[13])
	}

	// STMIB r6, {r0, r1}（ライトバックなし）: 格納先は base+4, base+8
	c.regs[6] = 0x3100
	mustStep(t, c, mem, 0xE9860003)
	if v, _ := mem.Read32(0x3104); v != 0xAAAA0000 {
		t.Errorf("STMIB: mem[3104] = %08X", v)
	}
	if v, _ := mem.Read32(0x3108); v != 0xBBBB1111 {
		t.Errorf("STMIB: mem[3108] = %08X", v)
	}
	if c.regs[6] != 0x3100 {
		t.Errorf("STMIB no-wb: r6 = %08X", c.regs[6])
	}

	// LDMDA r6!, {r7, r8}: base-4, base の順ではなく「小さいレジスタ=小さいアドレス」
	c.regs[6] = 0x3108
	mustStep(t, c, mem, 0xE8360180)
	if c.regs[7] != 0xAAAA0000 || c.regs[8] != 0xBBBB1111 {
		t.Errorf("LDMDA: r7,r8 = %08X %08X", c.regs[7], c.regs[8])
	}
	if c.regs[6] != 0x3100 {
		t.Errorf("LDMDA wb: r6 = %08X", c.regs[6])
	}
}

func TestLdmToPC(t *testing.T) {
	c, mem := newTestCore()
	_ = mem.Write32(0x3000, 0x12345678)
	_ = mem.Write32(0x3004, 0x00004000) // → PC
	c.regs[13] = 0x3000
	// LDMIA r13!, {r0, pc}
	mustStep(t, c, mem, 0xE8BD8001)
	if c.regs[0] != 0x12345678 || c.PC() != 0x4000 || c.regs[13] != 0x3008 {
		t.Errorf("LDMIA {r0,pc}: r0=%08X PC=%08X sp=%08X", c.regs[0], c.PC(), c.regs[13])
	}
}

// ---- MRS/MSR とモード切替・バンク ----

func TestMSRModeSwitchBanking(t *testing.T) {
	c, mem := newTestCore() // Reset 直後は SVC モード
	if c.cpsr.Mode() != ModeSvc {
		t.Fatalf("initial mode = %02X", c.cpsr.Mode())
	}
	c.regs[13] = 0x1111 // SVC の sp
	c.regs[14] = 0x2222 // SVC の lr

	// MSR CPSR_c, #ModeIrq|I|F → IRQ モードへ
	c.regs[4] = uint32(ModeIrq) | 0xC0
	mustStep(t, c, mem, 0xE121F004) // MSR CPSR_c, r4
	if c.cpsr.Mode() != ModeIrq {
		t.Fatalf("mode = %02X, want IRQ", c.cpsr.Mode())
	}
	// IRQ の r13/r14 は独立（初期値 0）
	if c.regs[13] != 0 || c.regs[14] != 0 {
		t.Errorf("IRQ bank r13/r14 = %08X/%08X, want 0/0", c.regs[13], c.regs[14])
	}
	c.regs[13] = 0x3333

	// SVC へ戻ると r13/r14 が復元される
	c.regs[4] = uint32(ModeSvc) | 0xC0
	mustStep(t, c, mem, 0xE121F004)
	if c.regs[13] != 0x1111 || c.regs[14] != 0x2222 {
		t.Errorf("SVC bank restore: r13/r14 = %08X/%08X", c.regs[13], c.regs[14])
	}

	// r0-r7 はバンクされない
	c.regs[0] = 0x7777
	c.regs[4] = uint32(ModeIrq) | 0xC0
	mustStep(t, c, mem, 0xE121F004)
	if c.regs[0] != 0x7777 {
		t.Errorf("r0 should not be banked: %08X", c.regs[0])
	}
	if c.regs[13] != 0x3333 {
		t.Errorf("IRQ bank r13 = %08X, want 3333", c.regs[13])
	}
}

func TestFIQBanking(t *testing.T) {
	c, mem := newTestCore()
	c.regs[8] = 0x88
	c.regs[12] = 0xCC
	// FIQ モードへ: r8-r12 も切り替わる
	c.regs[4] = uint32(ModeFiq) | 0xC0
	mustStep(t, c, mem, 0xE121F004)
	if c.regs[8] != 0 || c.regs[12] != 0 {
		t.Errorf("FIQ r8/r12 = %08X/%08X, want 0/0", c.regs[8], c.regs[12])
	}
	c.regs[8] = 0xF8
	// 戻す
	c.regs[4] = uint32(ModeSvc) | 0xC0
	mustStep(t, c, mem, 0xE121F004)
	if c.regs[8] != 0x88 || c.regs[12] != 0xCC {
		t.Errorf("restored r8/r12 = %08X/%08X", c.regs[8], c.regs[12])
	}
}

func TestMRS(t *testing.T) {
	c, mem := newTestCore()
	setFlags(c, "NC")
	mustStep(t, c, mem, 0xE10F0000) // MRS r0, CPSR
	want := uint32(FlagN|FlagC|FlagI|FlagF) | ModeSvc
	if c.regs[0] != want {
		t.Errorf("MRS: r0 = %08X, want %08X", c.regs[0], want)
	}
}

func TestMSRUserModeRestriction(t *testing.T) {
	c, mem := newTestCore()
	// まず usr モードに落とす（特権の SVC から）
	c.regs[4] = ModeUsr | 0xC0
	mustStep(t, c, mem, 0xE121F004)
	if c.cpsr.Mode() != ModeUsr {
		t.Fatalf("mode = %02X, want usr", c.cpsr.Mode())
	}
	// usr モードから MSR CPSR_c でモードを変えようとしても無視される
	c.regs[4] = uint32(ModeSvc)
	mustStep(t, c, mem, 0xE121F004)
	if c.cpsr.Mode() != ModeUsr {
		t.Errorf("user-mode MSR changed mode to %02X", c.cpsr.Mode())
	}
	// フラグは書ける: MSR CPSR_f, r4
	c.regs[4] = uint32(FlagN | FlagZ)
	mustStep(t, c, mem, 0xE128F004)
	if !c.cpsr.N() || !c.cpsr.Z() {
		t.Errorf("user-mode MSR flags failed: %v", c.cpsr)
	}
}

// ---- SWI（例外エントリ）----

func TestSWI(t *testing.T) {
	c, mem := newTestCore()
	// いったん IRQ モードにして、SWI で SVC に入ることを確認
	c.regs[4] = uint32(ModeIrq) | 0xC0
	mustStep(t, c, mem, 0xE121F004)
	oldCPSR := c.cpsr

	mustStep(t, c, mem, 0xEF000042) // SWI #0x42
	if c.cpsr.Mode() != ModeSvc {
		t.Errorf("SWI: mode = %02X, want SVC", c.cpsr.Mode())
	}
	if c.PC() != VecSWI {
		t.Errorf("SWI: PC = %08X, want %08X", c.PC(), uint32(VecSWI))
	}
	if want := uint32(testPC + 4 + 4); c.regs[14] != want {
		t.Errorf("SWI: lr = %08X, want %08X", c.regs[14], want)
	}
	if c.SPSR() != oldCPSR {
		t.Errorf("SWI: SPSR = %v, want %v", c.SPSR(), oldCPSR)
	}
	if c.cpsr&FlagI == 0 {
		t.Error("SWI: I flag not set")
	}
}

// ---- 例外復帰イディオム ----

func TestExceptionReturn(t *testing.T) {
	c, mem := newTestCore()
	// SWI で SVC に入り、SPSR に IRQ モードを持たせてから MOVS pc, lr
	c.regs[4] = uint32(ModeIrq) | 0xC0
	mustStep(t, c, mem, 0xE121F004)
	mustStep(t, c, mem, 0xEF000000) // SWI → SVC, lr=testPC+8, SPSR=IRQ

	mustStep(t, c, mem, 0xE1B0F00E) // MOVS pc, lr
	if c.cpsr.Mode() != ModeIrq {
		t.Errorf("MOVS pc,lr: mode = %02X, want IRQ", c.cpsr.Mode())
	}
	if want := uint32(testPC + 8); c.PC() != want {
		t.Errorf("MOVS pc,lr: PC = %08X, want %08X", c.PC(), want)
	}
}

// ---- 未実装命令の報告 ----

func TestUndefinedReportsPCAndWord(t *testing.T) {
	c, mem := newTestCore()
	const mul = 0xE0000291 // MUL r0, r1, r2（未実装）
	err := stepOne(t, c, mem, mul)
	var ue *UndefinedError
	if !errors.As(err, &ue) {
		t.Fatalf("err = %v, want UndefinedError", err)
	}
	if ue.PC != testPC || ue.Word != mul {
		t.Errorf("UndefinedError PC=%08X Word=%08X, want %08X/%08X", ue.PC, ue.Word, uint32(testPC), uint32(mul))
	}
	// PC は命令位置に戻っている（停止位置の報告用）
	if c.PC() != testPC {
		t.Errorf("PC after error = %08X, want %08X", c.PC(), uint32(testPC))
	}
}
