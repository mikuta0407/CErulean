package arm

import (
	"testing"

	"github.com/mikuta0407/cerulean/cpu"
)

// fakeCP15 は例外配送テスト用の CP15。FSR/FAR の書き込みを記録し、
// VectorBase を切り替えられる。
type fakeCP15 struct {
	vecBase  uint32
	fsr, far uint32
	priv     bool
}

func (f *fakeCP15) Read(opc1, crn, crm, opc2 uint8) (uint32, error) { return 0, nil }
func (f *fakeCP15) Write(opc1, crn, crm, opc2 uint8, v uint32) error {
	switch crn {
	case 5:
		f.fsr = v
	case 6:
		f.far = v
	}
	return nil
}
func (f *fakeCP15) VectorBase() uint32   { return f.vecBase }
func (f *fakeCP15) SetPrivileged(p bool) { f.priv = p }

// abortMem は特定アドレスへのアクセスで AbortError を返すメモリ。
type abortMem struct {
	testMem
	badAddr uint32
}

func (m *abortMem) abortIf(a uint32, write bool) error {
	if a&^3 == m.badAddr&^3 {
		return &cpu.AbortError{VA: a, Status: 0x5, Domain: 2, Write: write}
	}
	return nil
}
func (m *abortMem) Read32(a uint32) (uint32, error) {
	if err := m.abortIf(a, false); err != nil {
		return 0, err
	}
	return m.testMem.Read32(a)
}
func (m *abortMem) Write32(a uint32, v uint32) error {
	if err := m.abortIf(a, true); err != nil {
		return err
	}
	return m.testMem.Write32(a, v)
}

func newAbortCore(badAddr uint32) (*Core, *abortMem, *fakeCP15) {
	mem := &abortMem{testMem: testMem{data: make([]byte, 64*1024)}, badAddr: badAddr}
	cp := &fakeCP15{}
	c := New(mem, cp)
	c.Reset(testPC)
	return c, mem, cp
}

func TestDataAbortDelivery(t *testing.T) {
	c, mem, cp := newAbortCore(0x8000)
	c.SetReg(1, 0x8000)
	// LDR r0, [r1] → データアボート
	if err := stepOne(t, c, &mem.testMem, 0xE5910000); err != nil {
		t.Fatalf("abort should be delivered as exception, got error: %v", err)
	}
	if c.cpsr.Mode() != ModeAbt {
		t.Errorf("mode = %02X, want ABT", c.cpsr.Mode())
	}
	if c.PC() != VecDabt {
		t.Errorf("PC = %08X, want %08X", c.PC(), uint32(VecDabt))
	}
	if want := uint32(testPC + 8); c.Reg(14) != want {
		t.Errorf("LR = %08X, want %08X (PC+8)", c.Reg(14), want)
	}
	if cp.far != 0x8000 {
		t.Errorf("FAR = %08X, want 8000", cp.far)
	}
	if want := uint32(2<<4 | 0x5); cp.fsr != want {
		t.Errorf("FSR = %08X, want %08X (domain<<4|status)", cp.fsr, want)
	}
	// SPSR に元の CPSR（SVC）が入っている
	if c.SPSR().Mode() != ModeSvc {
		t.Errorf("SPSR mode = %02X, want SVC", c.SPSR().Mode())
	}
	// I ビットが立ち、アボートは ARM state で受ける
	if c.cpsr&FlagI == 0 || c.cpsr.T() {
		t.Errorf("CPSR after abort = %v", c.cpsr)
	}
}

func TestPrefetchAbortDelivery(t *testing.T) {
	c, _, cp := newAbortCore(0x8000)
	c.SetReg(15, 0x8000) // 実行不能アドレスへ飛んだ状態
	if err := c.Step(); err != nil {
		t.Fatalf("prefetch abort should be delivered, got: %v", err)
	}
	if c.cpsr.Mode() != ModeAbt {
		t.Errorf("mode = %02X, want ABT", c.cpsr.Mode())
	}
	if c.PC() != VecPabt {
		t.Errorf("PC = %08X, want %08X", c.PC(), uint32(VecPabt))
	}
	if want := uint32(0x8000 + 4); c.Reg(14) != want {
		t.Errorf("LR = %08X, want %08X (PC+4)", c.Reg(14), want)
	}
	// プリフェッチアボートでは FSR/FAR は更新されない
	if cp.far != 0 || cp.fsr != 0 {
		t.Errorf("FSR/FAR should not be updated on prefetch abort: %08X/%08X", cp.fsr, cp.far)
	}
}

func TestHighVectors(t *testing.T) {
	c, mem, cp := newAbortCore(0x8000)
	cp.vecBase = 0xFFFF0000
	c.SetReg(1, 0x8000)
	if err := stepOne(t, c, &mem.testMem, 0xE5910000); err != nil {
		t.Fatal(err)
	}
	if want := uint32(0xFFFF0000 | VecDabt); c.PC() != want {
		t.Errorf("PC = %08X, want %08X (high vectors)", c.PC(), want)
	}
}

func TestIRQDelivery(t *testing.T) {
	c, mem := newTestCore()
	// I ビットを落とす（Reset 直後は禁止されている）
	c.SetCPSR(c.CPSR() &^ FlagI)
	c.SetIRQ(true)
	if err := c.Step(); err != nil {
		t.Fatal(err)
	}
	if c.cpsr.Mode() != ModeIrq {
		t.Errorf("mode = %02X, want IRQ", c.cpsr.Mode())
	}
	if c.PC() != VecIRQ {
		t.Errorf("PC = %08X, want %08X", c.PC(), uint32(VecIRQ))
	}
	if want := uint32(testPC + 4); c.Reg(14) != want {
		t.Errorf("LR = %08X, want %08X (未実行命令+4)", c.Reg(14), want)
	}
	if c.cpsr&FlagI == 0 {
		t.Error("I should be set after IRQ entry")
	}
	// F は変化しない
	if c.cpsr&FlagF == 0 {
		t.Error("F should be preserved on IRQ entry")
	}
	_ = mem
}

func TestIRQMaskedByIFlag(t *testing.T) {
	c, mem := newTestCore()
	c.SetIRQ(true)                  // I=1 のままなので配送されない
	mustStep(t, c, mem, 0xE1A00000) // MOV r0, r0 (NOP)
	if c.cpsr.Mode() != ModeSvc {
		t.Errorf("mode = %02X, want SVC (IRQ masked)", c.cpsr.Mode())
	}
	if c.PC() != testPC+4 {
		t.Errorf("PC = %08X, want %08X", c.PC(), uint32(testPC+4))
	}
}

func TestFIQBeatsIRQ(t *testing.T) {
	c, _ := newTestCore()
	c.SetCPSR(c.CPSR() &^ (FlagI | FlagF))
	c.SetIRQ(true)
	c.SetFIQ(true)
	if err := c.Step(); err != nil {
		t.Fatal(err)
	}
	if c.cpsr.Mode() != ModeFiq {
		t.Errorf("mode = %02X, want FIQ", c.cpsr.Mode())
	}
	if c.PC() != VecFIQ {
		t.Errorf("PC = %08X, want %08X", c.PC(), uint32(VecFIQ))
	}
	// FIQ エントリでは I/F 両方が禁止される
	if c.cpsr&FlagI == 0 || c.cpsr&FlagF == 0 {
		t.Errorf("I/F should both be set after FIQ entry: %v", c.cpsr)
	}
}

func TestLdmAbortRestoresBase(t *testing.T) {
	// base restored モデル: ライトバック済みでもアボート時はベースを戻す。
	c, mem, _ := newAbortCore(0x8000)
	c.SetReg(1, 0x7FF8)                                             // r1 を起点に 4 ワードロード → 3 ワード目 (0x8000) でアボート
	if err := stepOne(t, c, &mem.testMem, 0xE8B1003C); err != nil { // LDMIA r1!, {r2-r5}
		t.Fatalf("abort should be delivered: %v", err)
	}
	if c.cpsr.Mode() != ModeAbt {
		t.Fatalf("mode = %02X, want ABT", c.cpsr.Mode())
	}
	// ABT モードから SVC バンクの r1 は見える（r1 はバンクされない）
	if got := c.Reg(1); got != 0x7FF8 {
		t.Errorf("base r1 = %08X, want 7FF8 (restored)", got)
	}
}

func TestLdmStmUserBank(t *testing.T) {
	c, mem := newTestCore()
	// SVC モードで usr の r13/r14 に値を仕込む: 一旦 SYS に切り替えて書き、SVC に戻す
	c.SetCPSR(PSR(ModeSys) | FlagI | FlagF)
	c.SetReg(13, 0x1111)
	c.SetReg(14, 0x2222)
	c.SetReg(8, 0x3333)
	c.SetCPSR(PSR(ModeSvc) | FlagI | FlagF)
	c.SetReg(13, 0xAAAA) // SVC 側の r13/r14 は別値
	c.SetReg(14, 0xBBBB)
	c.SetReg(0, 0x4000)

	// STMIA r0, {r8,r13,r14}^ → usr バンクの値が格納される
	mustStep(t, c, mem, 0xE8C06100)
	for i, want := range []uint32{0x3333, 0x1111, 0x2222} {
		if v, _ := mem.Read32(0x4000 + uint32(i)*4); v != want {
			t.Errorf("STM(2) word %d = %08X, want %08X", i, v, want)
		}
	}

	// メモリを書き換えて LDMIA r0, {r8,r13,r14}^ → usr バンクに入り、SVC 側は不変
	for i, v := range []uint32{0x5555, 0x6666, 0x7777} {
		if err := mem.Write32(0x4000+uint32(i)*4, v); err != nil {
			t.Fatal(err)
		}
	}
	mustStep(t, c, mem, 0xE8D06100)
	if c.Reg(13) != 0xAAAA || c.Reg(14) != 0xBBBB {
		t.Errorf("SVC r13/r14 changed: %08X/%08X", c.Reg(13), c.Reg(14))
	}
	c.SetCPSR(PSR(ModeSys) | FlagI | FlagF)
	if c.Reg(8) != 0x5555 || c.Reg(13) != 0x6666 || c.Reg(14) != 0x7777 {
		t.Errorf("usr r8/r13/r14 = %08X/%08X/%08X, want 5555/6666/7777",
			c.Reg(8), c.Reg(13), c.Reg(14))
	}
}

func TestStmUserBankFromFiq(t *testing.T) {
	// FIQ モードでは r8-r12 も usr バンク側が格納される
	c, mem := newTestCore()
	c.SetReg(8, 0x1234) // SVC(=usr と共有の r8) に値
	c.SetCPSR(PSR(ModeFiq) | FlagI | FlagF)
	c.SetReg(8, 0xFFFF) // FIQ バンクの r8
	c.SetReg(0, 0x4000)
	mustStep(t, c, mem, 0xE8C00100) // STMIA r0, {r8}^
	if v, _ := mem.Read32(0x4000); v != 0x1234 {
		t.Errorf("STM(2) from FIQ stored %08X, want 1234 (usr bank)", v)
	}
}

func TestArchUndefinedDelivery(t *testing.T) {
	// 存在しないコプロセッサへの MRC は実機同様に未定義命令例外になる
	//（WinCE の FPU 検出が依存する）。
	c, mem := newTestCore()
	if err := stepOne(t, c, mem, 0xEEF80A10); err != nil { // MRC p10（VFP）
		t.Fatalf("arch-undefined should be delivered as exception: %v", err)
	}
	if c.cpsr.Mode() != ModeUnd {
		t.Errorf("mode = %02X, want UND", c.cpsr.Mode())
	}
	if c.PC() != VecUndef {
		t.Errorf("PC = %08X, want %08X", c.PC(), uint32(VecUndef))
	}
	if want := uint32(testPC + 4); c.Reg(14) != want {
		t.Errorf("LR = %08X, want %08X", c.Reg(14), want)
	}
}
