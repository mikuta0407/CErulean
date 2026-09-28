package smdk2410

import (
	"testing"

	"github.com/mikuta0407/cerulean/snapshot/snapshottest"
)

func TestKbdStateFields(t *testing.T) {
	snapshottest.CheckFields(t, kbdMCU{}, []string{"out"}, []string{"raise", "log"})
}

// 1 バイトにつき EINT1 を 1 回: 積んだ時点で 1 回、ドライバが 1 バイト
// 読むごとに残りがあればもう 1 回。
func TestKbdOneInterruptPerByte(t *testing.T) {
	raised := 0
	k := &kbdMCU{raise: func() { raised++ }}
	k.push(0x5A)
	k.push(0xDA)
	if raised != 1 {
		t.Fatalf("raised %d times after two pushes, want 1 (second waits for the first to be read)", raised)
	}
	if b := k.Transfer(0xFF); b != 0x5A || raised != 2 {
		t.Fatalf("first byte %02X, raised %d; want 5A, 2", b, raised)
	}
	if b := k.Transfer(0xFF); b != 0xDA || raised != 2 {
		t.Fatalf("second byte %02X, raised %d; want DA, 2", b, raised)
	}
	if b := k.Transfer(0xFF); b != 0 {
		t.Errorf("empty queue returned %02X, want 0", b)
	}
}

func TestKeyDownUpBytes(t *testing.T) {
	m, err := New(nil)
	if err != nil {
		t.Fatal(err)
	}
	for _, tt := range []struct {
		key  string
		code byte
	}{{"Enter", 0x5A}, {"Up", 0x6C}, {"Down", 0x6A}, {"Left", 0x6D}, {"Right", 0x6F}} {
		if err := m.KeyDown(tt.key); err != nil {
			t.Fatal(err)
		}
		if err := m.KeyUp(tt.key); err != nil {
			t.Fatal(err)
		}
		if a, b := m.kbd.Transfer(0xFF), m.kbd.Transfer(0xFF); a != tt.code || b != tt.code|0x80 {
			t.Errorf("%s: bytes %02X %02X, want %02X %02X", tt.key, a, b, tt.code, tt.code|0x80)
		}
	}
	if err := m.KeyDown("SoftL"); err == nil {
		t.Error("unknown key accepted")
	}
	// EINT1 が INTC に届いていること
	m.KeyDown("Enter")
	if v, _ := m.Bus().Read32(0x4A000000); v&(1<<1) == 0 {
		t.Errorf("SRCPND = %08X, EINT1 not raised", v)
	}
}
