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

func TestMapRAMMirror(t *testing.T) {
	b := New()
	if err := b.MapRAMMirror("m", 0x1000, 0x400, 0x100); err != nil {
		t.Fatal(err)
	}
	if err := b.Write32(0x1004, 0xCAFEBABE); err != nil {
		t.Fatal(err)
	}
	// 0x100 ごとに折り返して同じ値が見える
	for _, a := range []uint32{0x1004, 0x1104, 0x1204, 0x1304} {
		if v, err := b.Read32(a); err != nil || v != 0xCAFEBABE {
			t.Errorf("Read32(%X) = %08X, %v; want CAFEBABE", a, v, err)
		}
	}
	// エイリアス先への書き込みは元にも見える
	if err := b.Write32(0x1204, 0x11111111); err != nil {
		t.Fatal(err)
	}
	if v, _ := b.Read32(0x1004); v != 0x11111111 {
		t.Errorf("alias write: Read32(0x1004) = %08X", v)
	}
	// 窓の外は BusError
	if _, err := b.Read32(0x1400); err == nil {
		t.Error("outside window should be BusError")
	}
	// size が 2 の冪でない場合は拒否
	if err := New().MapRAMMirror("bad", 0, 0x400, 0x300); err == nil {
		t.Error("non power-of-two size should be rejected")
	}
}

func TestWatch(t *testing.T) {
	b := New()
	if err := b.MapRAM("ram", 0x1000, 0x100); err != nil {
		t.Fatal(err)
	}
	dev := &stubDev{readVal: 0x55}
	if err := b.MapMMIO("dev", 0x2000, 0x100, dev); err != nil {
		t.Fatal(err)
	}
	type ev struct {
		name  string
		addr  uint32
		v     uint32
		write bool
	}
	var got []ev
	fn := func(name string, addr uint32, size int, v uint32, write bool) {
		got = append(got, ev{name, addr, v, write})
	}
	b.AddWatch(0x1010, 0x1013, fn)
	b.AddWatch(0x2000, 0x20FF, fn)

	_ = b.Write32(0x1010, 0x12345678) // 範囲内
	_ = b.Write32(0x1020, 1)          // 範囲外: 通知なし
	_, _ = b.Read8(0x1011)            // 範囲内
	_, _ = b.Read32(0x2004)           // MMIO 範囲内
	want := []ev{
		{"ram", 0x1010, 0x12345678, true},
		{"ram", 0x1011, 0x56, false},
		{"dev", 0x2004, 0x55, false},
	}
	if len(got) != len(want) {
		t.Fatalf("got %d events %+v, want %+v", len(got), got, want)
	}
	for i := range want {
		if got[i] != want[i] {
			t.Errorf("event %d = %+v, want %+v", i, got[i], want[i])
		}
	}
}

func TestUnalignedRegionAndRAMPage(t *testing.T) {
	b := New()
	if err := b.MapRAM("ram", 0x10000, 0x2000); err != nil {
		t.Fatal(err)
	}
	// 4KB に揃っていない小さな MMIO 2 個が同じページに同居する。
	d1, d2 := &stubDev{readVal: 1}, &stubDev{readVal: 2}
	if err := b.MapMMIO("d1", 0x20000, 0x10, d1); err != nil {
		t.Fatal(err)
	}
	if err := b.MapMMIO("d2", 0x20100, 0x10, d2); err != nil {
		t.Fatal(err)
	}
	if v, _ := b.Read32(0x20000); v != 1 {
		t.Errorf("d1 = %d", v)
	}
	if v, _ := b.Read32(0x20104); v != 2 {
		t.Errorf("d2 = %d", v)
	}
	if _, err := b.Read32(0x20080); err == nil {
		t.Error("gap between d1 and d2 must be unmapped")
	}

	_ = b.Write32(0x11004, 0xCAFEBABE)
	pg := b.RAMPage(0x11FFF)
	if len(pg) != 0x1000 || pg[4] != 0xBE {
		t.Fatalf("RAMPage = len %d", len(pg))
	}
	if b.RAMPage(0x20000) != nil {
		t.Error("RAMPage on MMIO must be nil")
	}
	b.AddWatch(0, 0, func(string, uint32, int, uint32, bool) {})
	if b.RAMPage(0x11000) != nil {
		t.Error("RAMPage must be nil while watching")
	}
}

// stableDev はオフセット 0 だけ StableRead で読めるデバイス。
type stableDev struct{ stubDev }

func (d *stableDev) StableRead(off uint32, size int) (uint32, bool) {
	if off != 0 {
		return 0, false
	}
	return d.Read(off, size), true
}

func TestProbe32(t *testing.T) {
	b := New()
	if err := b.MapRAM("ram", 0x1000, 0x1000); err != nil {
		t.Fatal(err)
	}
	sd := &stableDev{stubDev{readVal: 0x1234}}
	if err := b.MapMMIO("stable", 0x2000, 0x100, sd); err != nil {
		t.Fatal(err)
	}
	if err := b.MapMMIO("plain", 0x3000, 0x100, &stubDev{readVal: 1}); err != nil {
		t.Fatal(err)
	}
	_ = b.Write32(0x1010, 0xCAFEBABE)
	for _, c := range []struct {
		addr uint32
		v    uint32
		ok   bool
	}{
		{0x1010, 0xCAFEBABE, true}, // RAM
		{0x1012, 0, false},         // 非アライン
		{0x2000, 0x1234, true},     // StableRead できるレジスタ
		{0x2004, 0, false},         // StableRead が断るレジスタ
		{0x3000, 0, false},         // StableReader でないデバイス
		{0x9000, 0, false},         // 未マップ
	} {
		v, ok := b.Probe32(c.addr)
		if v != c.v || ok != c.ok {
			t.Errorf("Probe32(%X) = %X,%v, want %X,%v", c.addr, v, ok, c.v, c.ok)
		}
	}
	// 監視中は、実アクセスなら表示が出るので常に不可。
	b.AddWatch(0x9000, 0x9000, func(string, uint32, int, uint32, bool) {})
	if _, ok := b.Probe32(0x1010); ok {
		t.Error("Probe32 must fail while a watch is active")
	}
}
