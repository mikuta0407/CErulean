package loader

import (
	"bytes"
	"encoding/binary"
	"testing"
)

// makeBIN は合成 BIN イメージを組み立てるテストヘルパー。
// breakChecksum を true にすると最初のレコードのチェックサムを壊す。
func makeBIN(t *testing.T, start, length, entry uint32, recs []Segment, breakChecksum bool) []byte {
	t.Helper()
	var buf bytes.Buffer
	buf.WriteString("B000FF\x0A")
	writeU32 := func(v uint32) {
		var b [4]byte
		binary.LittleEndian.PutUint32(b[:], v)
		buf.Write(b[:])
	}
	writeU32(start)
	writeU32(length)
	for i, r := range recs {
		sum := byteSum(r.Data)
		if breakChecksum && i == 0 {
			sum++
		}
		writeU32(r.Addr)
		writeU32(uint32(len(r.Data)))
		writeU32(sum)
		buf.Write(r.Data)
	}
	// 終端レコード（実イメージの規則）: addr = 0, len = entry, checksum = 0
	writeU32(0)
	writeU32(entry)
	writeU32(0)
	return buf.Bytes()
}

func TestLoadBIN(t *testing.T) {
	recs := []Segment{
		{Addr: 0x80070000, Data: []byte{0x01, 0x02, 0x03, 0x04}},
		{Addr: 0x80100000, Data: bytes.Repeat([]byte{0xFF}, 16)},
	}
	raw := makeBIN(t, 0x80070000, 0x00200000, 0x80071000, recs, false)

	img, err := LoadBIN(bytes.NewReader(raw))
	if err != nil {
		t.Fatalf("LoadBIN: %v", err)
	}
	if img.Format != "bin" {
		t.Errorf("Format = %q, want %q", img.Format, "bin")
	}
	if img.Start != 0x80070000 || img.Length != 0x00200000 {
		t.Errorf("Start/Length = %08X/%08X, want 80070000/00200000", img.Start, img.Length)
	}
	if img.Entry != 0x80071000 {
		t.Errorf("Entry = %08X, want 80071000", img.Entry)
	}
	if len(img.Segs) != 2 {
		t.Fatalf("len(Segs) = %d, want 2", len(img.Segs))
	}
	for i, want := range recs {
		if img.Segs[i].Addr != want.Addr {
			t.Errorf("Segs[%d].Addr = %08X, want %08X", i, img.Segs[i].Addr, want.Addr)
		}
		if !bytes.Equal(img.Segs[i].Data, want.Data) {
			t.Errorf("Segs[%d].Data mismatch", i)
		}
	}
	if len(img.Records) != 2 {
		t.Fatalf("len(Records) = %d, want 2", len(img.Records))
	}
	if img.Records[1].Checksum != 16*0xFF {
		t.Errorf("Records[1].Checksum = %08X, want %08X", img.Records[1].Checksum, uint32(16*0xFF))
	}
}

func TestLoadBINChecksumMismatch(t *testing.T) {
	recs := []Segment{{Addr: 0x80070000, Data: []byte{0x01, 0x02}}}
	raw := makeBIN(t, 0x80070000, 0x1000, 0x80070000, recs, true)
	if _, err := LoadBIN(bytes.NewReader(raw)); err == nil {
		t.Fatal("LoadBIN accepted a broken checksum; want error")
	}
}

// {addr=entry, len=0} 形式の終端も受け付ける（別解釈のツール対策）。
func TestLoadBINLenZeroTerminator(t *testing.T) {
	var buf bytes.Buffer
	buf.WriteString("B000FF\x0A")
	for _, v := range []uint32{0x80070000, 0x10, // start, length
		0x80070000, 4, 10, // record: addr, len, checksum
	} {
		var b [4]byte
		binary.LittleEndian.PutUint32(b[:], v)
		buf.Write(b[:])
	}
	buf.Write([]byte{1, 2, 3, 4})                  // checksum 10
	for _, v := range []uint32{0x80071000, 0, 0} { // 終端: addr=entry, len=0
		var b [4]byte
		binary.LittleEndian.PutUint32(b[:], v)
		buf.Write(b[:])
	}
	img, err := LoadBIN(bytes.NewReader(buf.Bytes()))
	if err != nil {
		t.Fatal(err)
	}
	if img.Entry != 0x80071000 {
		t.Errorf("Entry = %08X, want 80071000", img.Entry)
	}
}

func TestLoadBINBadMagic(t *testing.T) {
	if _, err := LoadBIN(bytes.NewReader([]byte("NOTABIN\x0Axxxxxxxx"))); err == nil {
		t.Fatal("LoadBIN accepted bad magic; want error")
	}
}

func TestLoadBINTruncated(t *testing.T) {
	recs := []Segment{{Addr: 0x80070000, Data: []byte{0x01, 0x02, 0x03}}}
	raw := makeBIN(t, 0x80070000, 0x1000, 0x80070000, recs, false)
	// 終端レコードの途中で切る
	if _, err := LoadBIN(bytes.NewReader(raw[:len(raw)-6])); err == nil {
		t.Fatal("LoadBIN accepted a truncated image; want error")
	}
}

func TestLoadNB0(t *testing.T) {
	data := []byte{0xDE, 0xAD, 0xBE, 0xEF}
	img, err := LoadNB0(bytes.NewReader(data), 0x30000000)
	if err != nil {
		t.Fatalf("LoadNB0: %v", err)
	}
	if img.Format != "nb0" || img.Start != 0x30000000 || img.Entry != 0x30000000 || img.Length != 4 {
		t.Errorf("got Format=%q Start=%08X Entry=%08X Length=%d", img.Format, img.Start, img.Entry, img.Length)
	}
	if len(img.Segs) != 1 || !bytes.Equal(img.Segs[0].Data, data) {
		t.Error("Segs mismatch")
	}
}

func TestLoadNB0Empty(t *testing.T) {
	if _, err := LoadNB0(bytes.NewReader(nil), 0); err == nil {
		t.Fatal("LoadNB0 accepted empty image; want error")
	}
}
