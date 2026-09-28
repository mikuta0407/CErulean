package mmu

import (
	"errors"
	"testing"

	"github.com/mikuta0407/cerulean/bus"
	"github.com/mikuta0407/cerulean/cpu"
)

// テストは物理空間として実物の bus を使う（RAM 2MB、アドレス 0 起点）。
// L1 テーブルは testTTB（16KB アライン）、L2 テーブルは testL2 に置く。
const (
	testTTB = 0x8000
	testL2  = 0xC000
)

func setupMMU(t *testing.T) (*MMU, *bus.Bus) {
	t.Helper()
	b := bus.New()
	if err := b.MapRAM("ram", 0, 2<<20); err != nil {
		t.Fatal(err)
	}
	m := New(b)
	setCP15(t, m, 2, testTTB) // TTB
	setCP15(t, m, 3, 0x1)     // DACR: ドメイン0 = クライアント
	setCP15(t, m, 1, ctrlM)   // MMU 有効
	return m, b
}

func setCP15(t *testing.T, m *MMU, crn uint8, v uint32) {
	t.Helper()
	if err := m.Write(0, crn, 0, 0, v); err != nil {
		t.Fatal(err)
	}
}

// sectionDesc は一次セクション記述子を組み立てる。
func sectionDesc(pa, domain, ap uint32) uint32 {
	return pa&0xFFF00000 | ap<<10 | domain<<5 | 2
}

// smallDesc は二次小ページ記述子（4 サブページとも同じ AP）。
func smallDesc(pa, ap uint32) uint32 {
	return pa&0xFFFFF000 | ap<<10 | ap<<8 | ap<<6 | ap<<4 | 2
}

// setL1 は va に対応する一次エントリを書く。
func setL1(t *testing.T, b *bus.Bus, va, desc uint32) {
	t.Helper()
	if err := b.Write32(testTTB+(va>>20)*4, desc); err != nil {
		t.Fatal(err)
	}
}

// wantAbort は err が期待どおりの AbortError であることを確認する。
func wantAbort(t *testing.T, err error, status, domain uint8, write bool) {
	t.Helper()
	var ae *cpu.AbortError
	if !errors.As(err, &ae) {
		t.Fatalf("err = %v, want AbortError", err)
	}
	if ae.Status != status || ae.Domain != domain || ae.Write != write {
		t.Errorf("abort = {status=%X domain=%X write=%v}, want {%X %X %v}",
			ae.Status, ae.Domain, ae.Write, status, domain, write)
	}
}

func TestDisabledPassthrough(t *testing.T) {
	b := bus.New()
	if err := b.MapRAM("ram", 0, 0x1000); err != nil {
		t.Fatal(err)
	}
	m := New(b)
	if err := m.Write32(0x100, 0xCAFEF00D); err != nil {
		t.Fatal(err)
	}
	if v, _ := b.Read32(0x100); v != 0xCAFEF00D {
		t.Errorf("passthrough write: phys = %08X", v)
	}
}

func TestSectionTranslation(t *testing.T) {
	m, b := setupMMU(t)
	setL1(t, b, 0x80100000, sectionDesc(0x00100000, 0, 3))

	if err := m.Write32(0x80112344, 0xDEADBEEF); err != nil {
		t.Fatal(err)
	}
	if v, _ := b.Read32(0x00112344); v != 0xDEADBEEF {
		t.Errorf("phys[00112344] = %08X, want DEADBEEF", v)
	}
	if v, err := m.Read32(0x80112344); err != nil || v != 0xDEADBEEF {
		t.Errorf("virt read = %08X, %v", v, err)
	}
	if v, err := m.Read8(0x80112345); err != nil || v != 0xBE {
		t.Errorf("virt read8 = %02X, %v; want BE", v, err)
	}
}

func TestSectionTranslationFault(t *testing.T) {
	m, _ := setupMMU(t) // L1 は全部 0 = フォルト記述子
	_, err := m.Read32(0x80000000)
	wantAbort(t, err, fsTransSect, 0, false)
	err = m.Write32(0x80000000, 1)
	wantAbort(t, err, fsTransSect, 0, true)
}

func TestDomainFault(t *testing.T) {
	m, b := setupMMU(t)
	setL1(t, b, 0x80100000, sectionDesc(0x00100000, 3, 3)) // ドメイン3
	// DACR はドメイン0 のみ設定済み → ドメイン3 は 00 (no access)
	_, err := m.Read32(0x80100000)
	wantAbort(t, err, fsDomainSect, 3, false)
}

func TestManagerDomainSkipsAP(t *testing.T) {
	m, b := setupMMU(t)
	setL1(t, b, 0x80100000, sectionDesc(0x00100000, 2, 0)) // AP=00（アクセス不可相当）
	setCP15(t, m, 3, 3<<4)                                 // ドメイン2 = マネージャ
	if err := m.Write32(0x80100000, 1); err != nil {
		t.Errorf("manager domain should skip AP check: %v", err)
	}
}

func TestSectionPermissions(t *testing.T) {
	tests := []struct {
		name      string
		ap        uint32
		ctrl      uint32 // ctrlM に足す S/R
		priv      bool
		write     bool
		wantFault bool
	}{
		{"AP=01 特権RW", 1, 0, true, true, false},
		{"AP=01 ユーザー読み不可", 1, 0, false, false, true},
		{"AP=10 特権RW", 2, 0, true, true, false},
		{"AP=10 ユーザー読みOK", 2, 0, false, false, false},
		{"AP=10 ユーザー書き不可", 2, 0, false, true, true},
		{"AP=11 ユーザーRW", 3, 0, false, true, false},
		{"AP=00 S=0,R=0 特権読みも不可", 0, 0, true, false, true},
		{"AP=00 S=1 特権読みOK", 0, ctrlS, true, false, false},
		{"AP=00 S=1 特権書き不可", 0, ctrlS, true, true, true},
		{"AP=00 S=1 ユーザー読み不可", 0, ctrlS, false, false, true},
		{"AP=00 R=1 ユーザー読みOK", 0, ctrlR, false, false, false},
		{"AP=00 R=1 書き不可", 0, ctrlR, true, true, true},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			m, b := setupMMU(t)
			setL1(t, b, 0x80100000, sectionDesc(0x00100000, 0, tt.ap))
			setCP15(t, m, 1, ctrlM|tt.ctrl)
			m.SetPrivileged(tt.priv)

			var err error
			if tt.write {
				err = m.Write32(0x80100000, 1)
			} else {
				_, err = m.Read32(0x80100000)
			}
			if tt.wantFault {
				wantAbort(t, err, fsPermSect, 0, tt.write)
			} else if err != nil {
				t.Errorf("unexpected fault: %v", err)
			}
		})
	}
}

func TestCoarseSmallPage(t *testing.T) {
	m, b := setupMMU(t)
	// VA 0x80100000〜 を粗テーブル経由で 4KB ページにマップ。
	setL1(t, b, 0x80100000, testL2|0<<5|descCoarse)
	// idx0: PA 0x50000、idx1: フォルト（未設定）
	if err := b.Write32(testL2+0, smallDesc(0x00050000, 3)); err != nil {
		t.Fatal(err)
	}

	if err := m.Write32(0x80100123, 0x12345678); err != nil {
		t.Fatal(err)
	}
	if v, _ := b.Read32(0x00050123); v != 0x12345678 {
		t.Errorf("phys = %08X, want 12345678", v)
	}
	// 隣の 4KB はページ変換フォルト
	_, err := m.Read32(0x80101000)
	wantAbort(t, err, fsTransPage, 0, false)
}

func TestSmallPageSubpageAP(t *testing.T) {
	m, b := setupMMU(t)
	setL1(t, b, 0x80100000, testL2|0<<5|descCoarse)
	// ap0（オフセット 0x000-0x3FF）= 3（全モードRW）、ap1（0x400-0x7FF）= 1（特権のみ）
	desc := uint32(0x00050000) | 1<<6 | 3<<4 | 2
	if err := b.Write32(testL2+0, desc); err != nil {
		t.Fatal(err)
	}
	m.SetPrivileged(false)

	if _, err := m.Read32(0x80100000); err != nil {
		t.Errorf("subpage0 (AP=3) should be readable: %v", err)
	}
	_, err := m.Read32(0x80100400)
	wantAbort(t, err, fsPermPage, 0, false)
}

func TestLargePage(t *testing.T) {
	m, b := setupMMU(t)
	setL1(t, b, 0x80100000, testL2|0<<5|descCoarse)
	// VA 0x8010C123 → L2 idx = 0xC。大ページ PA 0x00060000。
	// AP サブフィールドは MVA[15:14]=3 → ap3（bits 11:10）だけ 3 にする。
	desc := uint32(0x00060000) | 3<<10 | descLarge
	if err := b.Write32(testL2+0xC*4, desc); err != nil {
		t.Fatal(err)
	}
	if err := m.Write32(0x8010C120, 0xA5A5A5A5); err != nil {
		t.Fatal(err)
	}
	if v, _ := b.Read32(0x0006C120); v != 0xA5A5A5A5 {
		t.Errorf("phys = %08X, want A5A5A5A5 (64KB ページ内オフセット維持)", v)
	}
}

func TestFineTinyPage(t *testing.T) {
	m, b := setupMMU(t)
	// 細テーブル（4KB アライン、1KB ページ）
	setL1(t, b, 0x80100000, testL2|0<<5|descFine)
	// VA 0x80100800 → idx = (0x800>>10) = 2
	desc := uint32(0x00070000) | 3<<4 | descTiny
	if err := b.Write32(testL2+2*4, desc); err != nil {
		t.Fatal(err)
	}
	if err := m.Write32(0x80100823, 0x77); err != nil {
		t.Fatal(err)
	}
	// 1KB ページなので PA = 0x70000 + (VA & 0x3FF)
	if v, _ := b.Read32(0x00070023); v != 0x77 {
		t.Errorf("phys = %08X, want 77", v)
	}
}

func TestFCSERemap(t *testing.T) {
	m, b := setupMMU(t)
	setCP15(t, m, 13, 1<<25) // PID = 1 → VA<32MB は MVA 0x02000000〜
	setL1(t, b, 0x02000000, sectionDesc(0x00100000, 0, 3))

	if err := m.Write32(0x00001234, 0xBEEF); err != nil {
		t.Fatal(err)
	}
	if v, _ := b.Read32(0x00101234); v != 0xBEEF {
		t.Errorf("FCSE remap: phys = %08X, want BEEF", v)
	}
	// 32MB 以上の VA は PID の影響を受けない
	setL1(t, b, 0x80100000, sectionDesc(0x00000000, 0, 3))
	if _, err := m.Read32(0x80100000); err != nil {
		t.Errorf("VA above 32MB should not be remapped: %v", err)
	}
}

func TestL1WalkBusErrorStops(t *testing.T) {
	// TTB が未マップ物理を指す場合は AbortError ではなく停止用エラー。
	b := bus.New()
	if err := b.MapRAM("ram", 0, 0x1000); err != nil {
		t.Fatal(err)
	}
	m := New(b)
	setCP15(t, m, 2, 0x100000) // RAM 外
	setCP15(t, m, 1, ctrlM)
	_, err := m.Read32(0x80000000)
	if err == nil {
		t.Fatal("want error")
	}
	var ae *cpu.AbortError
	if errors.As(err, &ae) {
		t.Errorf("L1 walk bus error should not be an AbortError: %v", err)
	}
}

func TestVectorBase(t *testing.T) {
	m, _ := setupMMU(t)
	if got := m.VectorBase(); got != 0 {
		t.Errorf("VectorBase = %08X, want 0", got)
	}
	setCP15(t, m, 1, ctrlM|ctrlV)
	if got := m.VectorBase(); got != 0xFFFF0000 {
		t.Errorf("VectorBase = %08X, want FFFF0000", got)
	}
}
