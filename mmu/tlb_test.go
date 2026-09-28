package mmu

import (
	"testing"

	"github.com/mikuta0407/cerulean/bus"
)

// TLB は c8 の無効化まで古い変換を保持する（実機と同じ。tlb.go 参照）。
func TestTLBStaleUntilFlush(t *testing.T) {
	m, b := setupMMU(t)
	_ = b.Write32(0x00100000, 0xAAAA)
	_ = b.Write32(0x00000000, 0xBBBB)
	setL1(t, b, 0x80100000, sectionDesc(0x00100000, 0, 3))
	if v, _ := m.Read32(0x80100000); v != 0xAAAA {
		t.Fatalf("first read = %X", v)
	}
	setL1(t, b, 0x80100000, sectionDesc(0x00000000, 0, 3)) // 無効化なしで張り替え
	if v, _ := m.Read32(0x80100000); v != 0xAAAA {
		t.Errorf("before c8: %X, want stale AAAA", v)
	}
	setCP15(t, m, 8, 0)
	if v, _ := m.Read32(0x80100000); v != 0xBBBB {
		t.Errorf("after c8: %X, want BBBB", v)
	}
}

// フォルトはキャッシュされないので、新規マッピングは無効化なしで見える。
func TestTLBDoesNotCacheFaults(t *testing.T) {
	m, b := setupMMU(t)
	if _, err := m.Read32(0x80100000); err == nil {
		t.Fatal("want fault on unmapped section")
	}
	setL1(t, b, 0x80100000, sectionDesc(0x00100000, 0, 3))
	if _, err := m.Read32(0x80100000); err != nil {
		t.Errorf("new mapping not visible: %v", err)
	}
}

// 特権で載せたエントリでも、ユーザーモードの権限チェックは効く。
func TestTLBPermissionPerMode(t *testing.T) {
	m, b := setupMMU(t)
	setL1(t, b, 0x80100000, sectionDesc(0x00100000, 0, 1)) // AP=01: 特権のみ
	if err := m.Write32(0x80100000, 1); err != nil {
		t.Fatal(err)
	}
	m.SetPrivileged(false)
	_, err := m.Read32(0x80100000)
	wantAbort(t, err, fsPermSect, 0, false)
	m.SetPrivileged(true)
	if _, err := m.Read32(0x80100000); err != nil {
		t.Errorf("privileged read after mode switch: %v", err)
	}
}

// DACR 変更（c3）で無効化され、ドメインフォルトが即座に効く。
func TestTLBFlushOnDACR(t *testing.T) {
	m, b := setupMMU(t)
	setL1(t, b, 0x80100000, sectionDesc(0x00100000, 0, 3))
	if _, err := m.Read32(0x80100000); err != nil {
		t.Fatal(err)
	}
	setCP15(t, m, 3, 0) // ドメイン0 = no access
	_, err := m.Read32(0x80100000)
	wantAbort(t, err, fsDomainSect, 0, false)
}

// FCSE: PID が変わると同じ VA でも別の MVA として引かれる（無効化不要）。
func TestTLBFCSETag(t *testing.T) {
	m, b := setupMMU(t)
	_ = b.Write32(0x00100000, 1)
	_ = b.Write32(0x00000000, 2)
	setL1(t, b, 0x02000000, sectionDesc(0x00100000, 0, 3)) // PID1 の slot
	setL1(t, b, 0x04000000, sectionDesc(0x00000000, 0, 3)) // PID2 の slot
	setCP15(t, m, 13, 1<<25)
	if v, _ := m.Read32(0); v != 1 {
		t.Errorf("PID1 read = %d, want 1", v)
	}
	setCP15(t, m, 13, 2<<25)
	if v, _ := m.Read32(0); v != 2 {
		t.Errorf("PID2 read = %d, want 2", v)
	}
}

type countDev struct{ reads int }

func (d *countDev) Read(off uint32, size int) uint32 { d.reads++; return off }
func (d *countDev) Write(off uint32, size int, v uint32) {}

// MMIO ページは TLB ヒットしても毎回デバイスに届く（RAM 直アクセスしない）。
func TestTLBMMIOGoesToDevice(t *testing.T) {
	b := bus.New()
	if err := b.MapRAM("ram", 0, 2<<20); err != nil {
		t.Fatal(err)
	}
	dev := &countDev{}
	if err := b.MapMMIO("dev", 0x4D000000, 0x1000, dev); err != nil {
		t.Fatal(err)
	}
	m := New(b)
	setCP15(t, m, 2, testTTB)
	setCP15(t, m, 3, 1)
	setCP15(t, m, 1, ctrlM)
	setL1(t, b, 0x90D00000, sectionDesc(0x4D000000, 0, 1))
	for i := 0; i < 3; i++ {
		if v, _ := m.Read32(0x90D00010); v != 0x10 {
			t.Fatalf("read = %X, want 10", v)
		}
	}
	if dev.reads != 3 {
		t.Errorf("device reads = %d, want 3", dev.reads)
	}
}
