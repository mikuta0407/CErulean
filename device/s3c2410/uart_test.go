package s3c2410

import (
	"bytes"
	"testing"
)

func TestUARTTransmit(t *testing.T) {
	var out bytes.Buffer
	u := NewUART(&out)
	for _, ch := range []byte("Hi\n") {
		u.Write(regUTXH, 1, uint32(ch))
	}
	if out.String() != "Hi\n" {
		t.Errorf("output = %q, want %q", out.String(), "Hi\n")
	}
}

func TestUARTStatusAlwaysReady(t *testing.T) {
	u := NewUART(nil)
	// bit1 (TX buffer empty) と bit2 (transmitter empty) が立っていること
	if v := u.Read(regUTRSTAT, 4); v&0x6 != 0x6 {
		t.Errorf("UTRSTAT = %X, want tx-empty bits set", v)
	}
	// RX ready (bit0) は立たない
	if v := u.Read(regUTRSTAT, 4); v&1 != 0 {
		t.Errorf("UTRSTAT = %X, rx-ready should be clear", v)
	}
}

func TestUARTConfigRegistersHoldValues(t *testing.T) {
	u := NewUART(nil)
	u.Write(regULCON, 4, 0x3)
	u.Write(regUBRDIV, 4, 0x1A)
	if v := u.Read(regULCON, 4); v != 0x3 {
		t.Errorf("ULCON = %X, want 3", v)
	}
	if v := u.Read(regUBRDIV, 4); v != 0x1A {
		t.Errorf("UBRDIV = %X, want 1A", v)
	}
}
