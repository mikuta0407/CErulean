package mmu

import (
	"testing"

	"github.com/mikuta0407/cerulean/bus"
	"github.com/mikuta0407/cerulean/snapshot/snapshottest"
)

func TestMMUStateFields(t *testing.T) {
	snapshottest.CheckFields(t, MMU{},
		[]string{"ctrl", "ttb", "dacr", "fsr", "far", "pid", "priv", "fetchGrace", "graceNext", "prevCtrl", "regs", "tlb"},
		[]string{"phys", "permR", "permW"})
}

func TestTLBEntryFields(t *testing.T) {
	// ram は pa から復元時に引き直す。
	snapshottest.CheckFields(t, tlbEntry{}, []string{"tag", "pa", "perm"}, []string{"ram"})
}

// 復元後の TLB が RAM の実体を指し直していること（fast path で読める）と、
// 状態が往復で変わらないことを確かめる。
func TestMMUStateRoundTrip(t *testing.T) {
	b := bus.New()
	if err := b.MapRAM("ram", 0x30000000, 0x10000); err != nil {
		t.Fatal(err)
	}
	src := New(b)
	src.ctrl, src.ttb, src.dacr, src.pid = 0x1234, 0x30004000, 1, 0x02000000
	src.fetchGrace, src.graceNext, src.prevCtrl = 1, 0x30000008, 0x78
	src.regs[9] = 0xABCD
	src.priv = false
	src.updatePermMask()
	// MMU 無効 = 恒等変換でページ 0x30001000 を TLB に載せる。
	src.ctrl = 0
	src.fill(0x30001000)
	src.ctrl = 0x1234 &^ ctrlM

	dst := New(b)
	snapshottest.RoundTrip(t, src, dst)
	if dst.permR != permUserR || dst.permW != permUserW {
		t.Errorf("perm mask not recomputed: %x %x", dst.permR, dst.permW)
	}
	e := &dst.tlb[(0x30001000>>12)&(tlbSize-1)]
	if e.tag&tlbValid == 0 || e.pa != 0x30001000 || e.ram == nil {
		t.Fatalf("TLB entry not restored: %+v", e)
	}
	if err := b.Write32(0x30001010, 0xCAFEF00D); err != nil {
		t.Fatal(err)
	}
	dst.ctrl = 0
	if v, err := dst.Read32(0x30001010); err != nil || v != 0xCAFEF00D {
		t.Errorf("read via restored TLB = %08X, %v", v, err)
	}
}
