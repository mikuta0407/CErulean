package bus

import (
	"bytes"
	"testing"

	"github.com/mikuta0407/cerulean/snapshot"

	"github.com/mikuta0407/cerulean/snapshot/snapshottest"
)

func TestBusStateFields(t *testing.T) {
	snapshottest.CheckFields(t, Bus{}, []string{"regions"}, []string{"pages", "watches", "hasWatch", "watchFn"})
	// region のうち状態は ram の中身だけ。他は構成。
	snapshottest.CheckFields(t, region{}, []string{"ram"}, []string{"base", "size", "name", "ramMask", "dev"})
}

func newTestBus(t *testing.T) *Bus {
	b := New()
	if err := b.MapRAM("a", 0x1000, 0x1000); err != nil {
		t.Fatal(err)
	}
	if err := b.MapMMIO("dev", 0x2000, 0x1000, nopDev{}); err != nil {
		t.Fatal(err)
	}
	if err := b.MapRAM("b", 0x3000, 0x2000); err != nil {
		t.Fatal(err)
	}
	return b
}

type nopDev struct{}

func (nopDev) Read(off uint32, size int) uint32     { return 0 }
func (nopDev) Write(off uint32, size int, v uint32) {}

func TestBusStateRoundTrip(t *testing.T) {
	src := newTestBus(t)
	_ = src.Write32(0x1004, 0x11223344)
	_ = src.Write32(0x4FFC, 0x55667788)
	dst := newTestBus(t)
	snapshottest.RoundTrip(t, src, dst)
	if v, _ := dst.Read32(0x1004); v != 0x11223344 {
		t.Errorf("RAM a = %08X", v)
	}
	if v, _ := dst.Read32(0x4FFC); v != 0x55667788 {
		t.Errorf("RAM b = %08X", v)
	}
}

func TestBusStateSizeMismatch(t *testing.T) {
	src := newTestBus(t)
	data := snapshottest.Save(t, src)
	dst := New()
	_ = dst.MapRAM("a", 0x1000, 0x1000)
	_ = dst.MapRAM("b", 0x3000, 0x1000) // 小さい
	r, err := snapshotReader(data)
	if err != nil {
		t.Fatal(err)
	}
	if err := r.Chunk("c", dst); err == nil {
		t.Error("loaded a snapshot into a bus with a different RAM size")
	}
}

func snapshotReader(data []byte) (*snapshot.Reader, error) {
	return snapshot.NewReader(bytes.NewReader(data))
}
