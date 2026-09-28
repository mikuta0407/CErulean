package smdk2410

import (
	"bytes"
	"testing"

	"github.com/mikuta0407/cerulean/loader"
	"github.com/mikuta0407/cerulean/snapshot/snapshottest"
)

func TestMachineStateFields(t *testing.T) {
	snapshottest.CheckFields(t, Machine{},
		[]string{"tickAcc", "steps", "entryPA"},
		// 各コンポーネントは自分のチャンクで保存される（snapshot.go）。
		[]string{"cpu", "bus", "mmu", "intc", "timer", "lcd", "rtc", "adc", "spi", "kbd"})
}

// loopProgram はタイマー 4 を自動リロードで回しながら（割り込みはマスク
// されたまま SRCPND に溜まる）、カウンタを RAM に書き続け、16 回ごとに
// UART1 へ 1 文字出すループ。RAM・タイマー・INTC・UART の状態が進み続ける。
func loopProgram() *loader.Image {
	prog := words(
		0xE3A01205, // MOV r1, #0x50000000
		0xE3811901, // ORR r1, r1, #0x4000      (UART1)
		0xE3811020, // ORR r1, r1, #0x20        (UTXH)
		0xE3A02203, // MOV r2, #0x30000000
		0xE3822601, // ORR r2, r2, #0x100000    (書き込み先 0x30100000)
		0xE3A04451, // MOV r4, #0x51000000      (PWM タイマー)
		0xE3A05FFA, // MOV r5, #1000
		0xE584503C, // STR r5, [r4, #0x3C]      (TCNTB4)
		0xE3A05606, // MOV r5, #0x600000        (manual update + auto reload)
		0xE5845008, // STR r5, [r4, #8]         (TCON)
		0xE3A05605, // MOV r5, #0x500000        (start + auto reload)
		0xE5845008, // STR r5, [r4, #8]
		// loop:
		0xE2800001, // ADD r0, r0, #1
		0xE4820004, // STR r0, [r2], #4
		0xE200303F, // AND r3, r0, #0x3F
		0xE3833040, // ORR r3, r3, #0x40
		0xE310000F, // TST r0, #0xF
		0x05C13000, // STRBEQ r3, [r1]
		0xEAFFFFF8, // B loop
	)
	return &loader.Image{
		Format: "bin", Start: 0x80070000, Length: uint32(len(prog)), Entry: 0x80070000,
		Segs: []loader.Segment{{Addr: 0x80070000, Data: prog}},
	}
}

func newLoopMachine(t *testing.T, out *bytes.Buffer) *Machine {
	t.Helper()
	m, err := New(out)
	if err != nil {
		t.Fatal(err)
	}
	if err := m.LoadImage(loopProgram()); err != nil {
		t.Fatal(err)
	}
	m.Reset()
	return m
}

func run(t *testing.T, m *Machine, n int) {
	t.Helper()
	for i := 0; i < n; i++ {
		if err := m.Step(); err != nil {
			t.Fatalf("step %d: %v", m.Steps(), err)
		}
	}
}

func save(t *testing.T, m *Machine) []byte {
	t.Helper()
	var buf bytes.Buffer
	if err := m.SaveSnapshot(&buf, "img"); err != nil {
		t.Fatal(err)
	}
	return buf.Bytes()
}

// 「N 命令 → 保存 → 新しいマシンに復元 → M 命令」と「通しで N+M 命令」で、
// UART 出力と最終状態（スナップショットのバイト列 = RAM・全レジスタ・
// 全デバイス）が一致すること。
func TestSnapshotResumeMatchesContinuousRun(t *testing.T) {
	const n, m = 30011, 40009 // ループ周期・タイマー周期と揃わない値

	var outA bytes.Buffer
	a := newLoopMachine(t, &outA)
	run(t, a, n)
	snap := save(t, a)
	uartAtSave := outA.Len()

	var outB bytes.Buffer
	b, err := New(&outB)
	if err != nil {
		t.Fatal(err)
	}
	id, err := b.LoadSnapshot(bytes.NewReader(snap))
	if err != nil {
		t.Fatal(err)
	}
	if id != "img" {
		t.Errorf("imageID = %q", id)
	}
	if b.Steps() != n {
		t.Errorf("restored steps = %d, want %d", b.Steps(), n)
	}
	// 保存→復元→保存でバイト列が変わらない。
	if !bytes.Equal(save(t, b), snap) {
		t.Fatal("save after load differs from the loaded snapshot")
	}

	run(t, a, m)
	run(t, b, m)

	if got, want := outB.String(), outA.String()[uartAtSave:]; got != want {
		t.Errorf("UART output after resume differs:\n got %q\nwant %q", got, want)
	}
	if len(outB.String()) == 0 {
		t.Error("test program produced no UART output after resume")
	}
	if !bytes.Equal(save(t, a), save(t, b)) {
		t.Error("final state differs between resumed and continuous runs")
	}
	for i := 0; i < 16; i++ {
		if a.CPU().Reg(i) != b.CPU().Reg(i) {
			t.Errorf("r%d: %08X vs %08X", i, a.CPU().Reg(i), b.CPU().Reg(i))
		}
	}
	// タイマー割り込みが実際に溜まっている（タイマー状態が検証に効いている）こと。
	if v, _ := a.Bus().Read32(0x4A000000); v&(1<<14) == 0 {
		t.Errorf("SRCPND = %08X, timer 4 never fired", v)
	}
}

func TestSnapshotTruncated(t *testing.T) {
	var buf bytes.Buffer
	a := newLoopMachine(t, &buf)
	snap := save(t, a)
	b := newLoopMachine(t, &buf)
	if _, err := b.LoadSnapshot(bytes.NewReader(snap[:20])); err == nil {
		t.Error("truncated snapshot loaded without error")
	}
}
