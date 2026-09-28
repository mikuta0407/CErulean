package smdk2410

import (
	"bytes"
	"encoding/binary"
	"errors"
	"testing"

	"github.com/mikuta0407/cerulean/cpu/arm"
	"github.com/mikuta0407/cerulean/loader"
)

func words(ws ...uint32) []byte {
	buf := make([]byte, 4*len(ws))
	for i, w := range ws {
		binary.LittleEndian.PutUint32(buf[4*i:], w)
	}
	return buf
}

// デバッグシリアル（UART1）に "OK" を出力してから停止する小さなプログラムを、
// CE 仮想アドレス（0x80070000）に置いたイメージとして実行する統合テスト。
func TestBootToUART(t *testing.T) {
	prog := words(
		0xE3A00205, // MOV r0, #0x50000000   (UART ベース)
		0xE3800901, // ORR r0, r0, #0x4000   (UART1)
		0xE3800020, // ORR r0, r0, #0x20     (UTXH)
		0xE3A0104F, // MOV r1, #'O'
		0xE5C01000, // STRB r1, [r0]
		0xE3A0104B, // MOV r1, #'K'
		0xE5C01000, // STRB r1, [r0]
		0xE8B10000, // 空リスト LDM（UNPREDICTABLE → エミュレーション停止するはず）
	)
	img := &loader.Image{
		Format: "bin",
		Start:  0x80070000,
		Length: uint32(len(prog)),
		Entry:  0x80070000,
		Segs:   []loader.Segment{{Addr: 0x80070000, Data: prog}},
	}

	var out bytes.Buffer
	m, err := New(&out)
	if err != nil {
		t.Fatal(err)
	}
	if err := m.LoadImage(img); err != nil {
		t.Fatal(err)
	}
	m.Reset()

	// エントリの VA→PA 変換: 0x80070000 → 0x30070000
	if pc := m.CPU().PC(); pc != 0x30070000 {
		t.Fatalf("reset PC = %08X, want 30070000", pc)
	}

	var stepErr error
	for i := 0; i < 100; i++ {
		if stepErr = m.Step(); stepErr != nil {
			break
		}
	}
	var ue *arm.UndefinedError
	if !errors.As(stepErr, &ue) {
		t.Fatalf("expected UndefinedError, got %v", stepErr)
	}
	if ue.PC != 0x3007001C {
		t.Errorf("stopped at PC=%08X, want 3007001C", ue.PC)
	}
	if out.String() != "OK" {
		t.Errorf("UART output = %q, want %q", out.String(), "OK")
	}
}

func TestVAToPA(t *testing.T) {
	tests := []struct {
		va      uint32
		want    uint32
		wantErr bool
	}{
		{0x80000000, 0x30000000, false}, // カーネルキャッシュ空間
		{0x80070000, 0x30070000, false},
		{0xA0070000, 0x30070000, false}, // 非キャッシュ空間も同じ物理へ
		{0x30001000, 0x30001000, false}, // 物理アドレス直指定はそのまま
		{0x00001000, 0, true},           // マップなし
		{0xC0000000, 0, true},
	}
	for _, tt := range tests {
		got, err := vaToPA(tt.va)
		if (err != nil) != tt.wantErr {
			t.Errorf("vaToPA(%08X): err = %v, wantErr=%v", tt.va, err, tt.wantErr)
			continue
		}
		if !tt.wantErr && got != tt.want {
			t.Errorf("vaToPA(%08X) = %08X, want %08X", tt.va, got, tt.want)
		}
	}
}

func TestLoadImageOutOfRange(t *testing.T) {
	m, err := New(nil)
	if err != nil {
		t.Fatal(err)
	}
	img := &loader.Image{
		Entry: 0x80000000,
		Segs:  []loader.Segment{{Addr: 0x87FFFFFC, Data: make([]byte, 16)}}, // RAM 末尾（128MB）を越える
	}
	if err := m.LoadImage(img); err == nil {
		t.Error("LoadImage accepted a segment beyond RAM; want error")
	}
}
