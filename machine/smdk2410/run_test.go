package smdk2410

import (
	"bytes"
	"encoding/binary"
	"strings"
	"testing"

	"github.com/mikuta0407/cerulean/cpu/arm"
	"github.com/mikuta0407/cerulean/loader"
)

// 合成プログラムは Go と Rust の一致確認で共用するため testdata の命令語
// ファイルに置いてある（内容と意図はファイルのコメント参照）。
const syntheticDir = "../../testdata/golden/synthetic/"

// loadWords は testdata の合成プログラムを読む。
func loadWords(t *testing.T, name string) *loader.Image {
	t.Helper()
	img, err := loader.Load(syntheticDir+name, 0)
	if err != nil {
		t.Fatal(err)
	}
	return img
}

// wordAt は img の va にある語（置かれていなければ 0）。
func wordAt(img *loader.Image, va uint32) uint32 {
	for _, s := range img.Segs {
		if va >= s.Addr && va+4 <= s.Addr+uint32(len(s.Data)) {
			return binary.LittleEndian.Uint32(s.Data[va-s.Addr:])
		}
	}
	return 0
}

// idleProgram は WinCE の OAL と同じ形のアイドル待ち（Timer4 の割り込みで
// RAM のフラグが立つのを 3 命令ループで待つ）。フラグは PA 0x30001000、
// ループを抜けた回数のカウンタは 0x30001004。
func idleProgram(t *testing.T) *loader.Image { return loadWords(t, "idle.words") }

func newIdleMachine(t *testing.T, skip bool) *Machine {
	t.Helper()
	m, err := New(nil)
	if err != nil {
		t.Fatal(err)
	}
	if err := m.LoadImage(idleProgram(t)); err != nil {
		t.Fatal(err)
	}
	m.SetIdleSkip(skip)
	m.Reset()
	return m
}

// 手で符号化した命令語が意図どおりか、逆アセンブラで確かめる（符号化ミスで
// テストが別のことを検査してしまうのを防ぐ）。
func TestIdleProgramEncoding(t *testing.T) {
	img := idleProgram(t)
	for _, c := range []struct {
		off  uint32
		want string
	}{
		{0x00, "b 0x00000100"}, {0x18, "b 0x00000200"},
		{0x164, "ldr r3, [r6]"}, {0x168, "cmp r3, #0x0"}, {0x16C, "beq 0x00000164"},
		{0x180, "b 0x00000164"}, {0x228, "subs pc, lr, #0x4"},
	} {
		w := wordAt(img, 0x80000000+c.off)
		if got := arm.Disasm(w, c.off); !strings.HasPrefix(got, c.want) {
			t.Errorf("%03X: %08X = %q, want %q", c.off, w, got, c.want)
		}
	}
}

// アイドルスキップの有無で、同じ命令数まで進めたときの全状態（スナップ
// ショットのバイト列）が一致すること。RunUntil の上限を不揃いに刻み、
// 上限・タイマー期限・割り込みの境界をまたぐ場合を含める。
func TestIdleSkipMatchesStepping(t *testing.T) {
	a := newIdleMachine(t, true)
	b := newIdleMachine(t, false)
	limit := uint64(0)
	for i, chunk := range []uint64{1, 150, 5000, 7, 12345, 3, 60000, 99991, 2, 150000} {
		limit += chunk
		if err := a.RunUntil(limit); err != nil {
			t.Fatalf("skip: %v", err)
		}
		if err := b.RunUntil(limit); err != nil {
			t.Fatalf("step: %v", err)
		}
		if a.Steps() != limit || b.Steps() != limit {
			t.Fatalf("steps = %d/%d, want %d", a.Steps(), b.Steps(), limit)
		}
		if !bytes.Equal(save(t, a), save(t, b)) {
			t.Fatalf("chunk %d (step %d): state differs between idle skip and stepping", i, limit)
		}
	}
	// 割り込みで実際にループを抜けていること（カウンタが進む）と、
	// 大半を飛ばしていること。
	ram, off, _ := a.bus.RAM(0x30001004)
	count := binary.LittleEndian.Uint32(ram[off:])
	if count < 50 {
		t.Errorf("counter = %d, want >= 50 (timer interrupts did not break the loop)", count)
	}
	if a.IdleSkipped() < limit/2 {
		t.Errorf("skipped %d of %d steps, want most of them", a.IdleSkipped(), limit)
	}
	if b.IdleSkipped() != 0 {
		t.Errorf("skip disabled but skipped %d", b.IdleSkipped())
	}
}

// 1 命令ずつ（Step）とまとめて（RunUntil）で結果が同じこと。
func TestStepMatchesRunUntil(t *testing.T) {
	a := newIdleMachine(t, true)
	b := newIdleMachine(t, true)
	const n = 40000
	for i := 0; i < n; i++ {
		if err := a.Step(); err != nil {
			t.Fatal(err)
		}
	}
	if err := b.RunUntil(n); err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(save(t, a), save(t, b)) {
		t.Fatal("Step x n and RunUntil(n) differ")
	}
}

// adcPollProgram は touch.dll と同じ形で ADC の変換完了を LDR/TST/BEQ の
// ループで待つ（MMIO のポーリング）。完了ごとにカウンタ r7 を増やす。
func adcPollProgram(t *testing.T) *loader.Image { return loadWords(t, "adc-poll.words") }

func TestADCPollProgramEncoding(t *testing.T) {
	img := adcPollProgram(t)
	for off, want := range map[uint32]string{
		0x14: "ldr r3, [r4]", 0x18: "tst r3, #0x8000", 0x1C: "beq 0x00000014", 0x24: "b 0x0000000C",
	} {
		w := wordAt(img, 0x80001000+off)
		if got := arm.Disasm(w, off); !strings.HasPrefix(got, want) {
			t.Errorf("%02X: %08X = %q, want %q", off, w, got, want)
		}
	}
}

// MMIO のポーリング（bus.StableReader）でも、スキップの有無で全状態が一致する。
func TestIdleSkipMMIOPollMatchesStepping(t *testing.T) {
	newM := func(skip bool) *Machine {
		m, err := New(nil)
		if err != nil {
			t.Fatal(err)
		}
		if err := m.LoadImage(adcPollProgram(t)); err != nil {
			t.Fatal(err)
		}
		m.SetIdleSkip(skip)
		m.Reset()
		return m
	}
	a, b := newM(true), newM(false)
	limit := uint64(0)
	for i, chunk := range []uint64{7, 100000, 31, 250000, 4, 400001} {
		limit += chunk
		if err := a.RunUntil(limit); err != nil {
			t.Fatal(err)
		}
		if err := b.RunUntil(limit); err != nil {
			t.Fatal(err)
		}
		if !bytes.Equal(save(t, a), save(t, b)) {
			t.Fatalf("chunk %d (step %d): state differs", i, limit)
		}
	}
	if r7 := a.CPU().Reg(7); r7 < 4 {
		t.Errorf("conversions completed = %d, want >= 4", r7)
	}
	if a.IdleSkipped() < limit/2 {
		t.Errorf("skipped %d of %d steps", a.IdleSkipped(), limit)
	}
}
