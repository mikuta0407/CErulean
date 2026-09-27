package bus

import (
	"errors"
	"testing"
)

type stubDev struct {
	lastOff  uint32
	lastSize int
	lastVal  uint32
	readVal  uint32
}

func (d *stubDev) Read(off uint32, size int) uint32 {
	d.lastOff, d.lastSize = off, size
	return d.readVal
}
func (d *stubDev) Write(off uint32, size int, v uint32) {
	d.lastOff, d.lastSize, d.lastVal = off, size, v
}

func TestRAMReadWrite(t *testing.T) {
	b := New()
	if err := b.MapRAM("ram", 0x1000, 0x100); err != nil {
		t.Fatal(err)
	}
	if err := b.Write32(0x1010, 0xAABBCCDD); err != nil {
		t.Fatal(err)
	}
	// リトルエンディアン確認
	if v, _ := b.Read8(0x1010); v != 0xDD {
		t.Errorf("Read8 = %02X, want DD", v)
	}
	if v, _ := b.Read16(0x1012); v != 0xAABB {
		t.Errorf("Read16 = %04X, want AABB", v)
	}
	if v, _ := b.Read32(0x1010); v != 0xAABBCCDD {
		t.Errorf("Read32 = %08X", v)
	}
}

func TestUnmappedAccess(t *testing.T) {
	b := New()
	_ = b.MapRAM("ram", 0x1000, 0x100)
	var be *BusError
	if _, err := b.Read32(0x2000); !errors.As(err, &be) || be.Addr != 0x2000 || be.Write {
		t.Errorf("read unmapped: err = %v", err)
	}
	if err := b.Write8(0x0FFF, 1); !errors.As(err, &be) || !be.Write {
		t.Errorf("write unmapped: err = %v", err)
	}
	// 領域末尾をまたぐアクセスもエラー
	if _, err := b.Read32(0x10FE); err == nil {
		t.Error("read crossing region end succeeded; want error")
	}
}

func TestMMIODispatch(t *testing.T) {
	b := New()
	dev := &stubDev{readVal: 0x1234}
	if err := b.MapMMIO("dev", 0x5000, 0x100, dev); err != nil {
		t.Fatal(err)
	}
	if v, err := b.Read16(0x5020); err != nil || v != 0x1234 {
		t.Errorf("MMIO read: v=%04X err=%v", v, err)
	}
	if dev.lastOff != 0x20 || dev.lastSize != 2 {
		t.Errorf("MMIO read dispatch: off=%X size=%d", dev.lastOff, dev.lastSize)
	}
	if err := b.Write32(0x5044, 0xCAFE); err != nil {
		t.Fatal(err)
	}
	if dev.lastOff != 0x44 || dev.lastSize != 4 || dev.lastVal != 0xCAFE {
		t.Errorf("MMIO write dispatch: off=%X size=%d val=%X", dev.lastOff, dev.lastSize, dev.lastVal)
	}
}

func TestOverlapRejected(t *testing.T) {
	b := New()
	_ = b.MapRAM("a", 0x1000, 0x100)
	if err := b.MapRAM("b", 0x10F0, 0x100); err == nil {
		t.Error("overlapping region accepted; want error")
	}
	if err := b.MapRAM("c", 0x1100, 0x100); err != nil {
		t.Errorf("adjacent region rejected: %v", err)
	}
}

func TestRAMLookup(t *testing.T) {
	b := New()
	_ = b.MapRAM("ram", 0x1000, 0x100)
	ram, off, ok := b.RAM(0x1040)
	if !ok || off != 0x40 || len(ram) != 0x100 {
		t.Errorf("RAM lookup: ok=%v off=%X len=%d", ok, off, len(ram))
	}
	if _, _, ok := b.RAM(0x2000); ok {
		t.Error("RAM lookup outside region succeeded")
	}
}
