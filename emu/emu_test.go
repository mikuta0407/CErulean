package emu

import (
	"bytes"
	"errors"
	"os"
	"testing"

	"github.com/mikuta0407/cerulean/loader"
	"github.com/mikuta0407/cerulean/machine"
	"github.com/mikuta0407/cerulean/machine/smdk2410"
	"github.com/mikuta0407/cerulean/script"
)

// spinMachine は「カウンタを増やし続けるだけ」のプログラムを載せたマシン。
// 入力は ADC・キーボード用マイコンの状態に残るので、スナップショットの
// バイト列で入力の適用時刻・内容の一致を比べられる。
func spinMachine(t *testing.T) *smdk2410.Machine {
	t.Helper()
	m, err := smdk2410.New(nil)
	if err != nil {
		t.Fatal(err)
	}
	prog := []byte{
		0x01, 0x00, 0x80, 0xE2, // ADD r0, r0, #1
		0xFD, 0xFF, 0xFF, 0xEA, // B -4 (ADD へ)
	}
	img := &loader.Image{Format: "bin", Start: 0x80000000, Length: 8, Entry: 0x80000000,
		Segs: []loader.Segment{{Addr: 0x80000000, Data: prog}}}
	if err := m.LoadImage(img); err != nil {
		t.Fatal(err)
	}
	m.Reset()
	return m
}

func snap(t *testing.T, m machine.Machine) []byte {
	t.Helper()
	var buf bytes.Buffer
	if err := m.SaveSnapshot(&buf, "id"); err != nil {
		t.Fatal(err)
	}
	return buf.Bytes()
}

// Run(until) は until ちょうどのイベントを適用せずに戻り、次の Run の
// 最初に適用する。予定は命令数順・同時刻は加えた順。
func TestRunEventBoundaries(t *testing.T) {
	m := spinMachine(t)
	s := New(m)
	var applied []string
	s.Apply = func(m machine.Machine, ev script.Event) (bool, error) {
		applied = append(applied, ev.Key)
		if m.Steps() != ev.Step {
			t.Errorf("%s applied at step %d, want %d", ev.Key, m.Steps(), ev.Step)
		}
		return ev.Kind == script.Quit, nil
	}
	s.Schedule(
		script.Event{Step: 100, Kind: script.KeyDown, Key: "b"},
		script.Event{Step: 50, Kind: script.KeyDown, Key: "a"},
		script.Event{Step: 100, Kind: script.KeyDown, Key: "c"},
		script.Event{Step: 200, Kind: script.Quit, Key: "quit"},
		script.Event{Step: 300, Kind: script.KeyDown, Key: "never"},
	)
	if quit, err := s.Run(100); quit || err != nil {
		t.Fatalf("Run(100) = %v, %v", quit, err)
	}
	if m.Steps() != 100 || len(applied) != 1 {
		t.Fatalf("after Run(100): steps %d, applied %v", m.Steps(), applied)
	}
	quit, err := s.Run(1000)
	if !quit || err != nil {
		t.Fatalf("Run(1000) = %v, %v", quit, err)
	}
	if m.Steps() != 200 {
		t.Errorf("stopped at %d, want 200", m.Steps())
	}
	if got := len(applied); got != 4 || applied[1] != "b" || applied[2] != "c" {
		t.Errorf("applied %v", applied)
	}
	if s.Pending() != 1 {
		t.Errorf("pending = %d", s.Pending())
	}
}

func TestRunEventError(t *testing.T) {
	s := New(spinMachine(t))
	s.Schedule(script.Event{Step: 10, Kind: script.Shot, Path: "x.png", Line: 7})
	_, err := s.Run(100)
	var ee *EventError
	if !errors.As(err, &ee) || ee.Event.Line != 7 {
		t.Fatalf("err = %v, want EventError for line 7", err)
	}
}

// 対話入力を記録 → Format → Parse → 別のマシンで再生、で同じ命令数での
// 全状態（スナップショット）が一致すること（決定論性の保証）。
func TestRecordReplay(t *testing.T) {
	a := spinMachine(t)
	start := snap(t, a)
	s := New(a)
	s.StartRecording()
	// 実際の対話と同じく、任意の命令数で Run を止めて入力を差し込む。
	inputs := []struct {
		at uint64
		ev script.Event
	}{
		{1000, script.Event{Kind: script.TouchDown, X: 20, Y: 10}},
		{1777, script.Event{Kind: script.TouchMove, X: 21, Y: 12}},
		{5003, script.Event{Kind: script.TouchUp}},
		{5003, script.Event{Kind: script.KeyDown, Key: "Right"}},
		{9001, script.Event{Kind: script.KeyUp, Key: "Right"}},
	}
	for _, in := range inputs {
		if _, err := s.Run(in.at); err != nil {
			t.Fatal(err)
		}
		if err := s.Inject(in.ev); err != nil {
			t.Fatal(err)
		}
	}
	if _, err := s.Run(20000); err != nil {
		t.Fatal(err)
	}
	recStart, rec := s.StopRecording()
	if recStart != 0 || len(rec) != len(inputs) || rec[2].Step != 5003 {
		t.Fatalf("recorded start=%d events=%+v", recStart, rec)
	}
	var text bytes.Buffer
	if err := Format(&text, "start.snap", "id", recStart, rec); err != nil {
		t.Fatal(err)
	}
	evs, err := script.Parse(&text, smdk2410.InstructionsPerSecond)
	if err != nil {
		t.Fatalf("%v\n%s", err, text.String())
	}

	b, err := smdk2410.New(nil)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := b.LoadSnapshot(bytes.NewReader(start)); err != nil {
		t.Fatal(err)
	}
	r := New(b)
	r.Schedule(evs...)
	if _, err := r.Run(20000); err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(snap(t, a), snap(t, b)) {
		t.Error("replayed state differs from the recorded session")
	}
}

func TestInjectRejects(t *testing.T) {
	s := New(spinMachine(t))
	for _, ev := range []script.Event{
		{Kind: script.Shot, Path: "x"},
		{Kind: script.TouchDown, X: 240, Y: 0},
		{Kind: script.KeyDown, Key: "NoSuchKey"},
	} {
		if err := s.Inject(ev); err == nil {
			t.Errorf("Inject(%+v) succeeded", ev)
		}
	}
}

// 実イメージでの記録 → 再生。環境変数 CERULEAN_TODAY_SNAP（Today 画面の
// スナップショット）があるときだけ走る。Start → Calendar をタップし、
// 記録を再生した画面とスナップショットが一致することを確かめる。
func TestRealImageRecordReplay(t *testing.T) {
	path := os.Getenv("CERULEAN_TODAY_SNAP")
	if path == "" {
		t.Skip("CERULEAN_TODAY_SNAP not set")
	}
	if testing.Short() {
		t.Skip("slow")
	}
	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	load := func() *smdk2410.Machine {
		m, err := smdk2410.New(nil)
		if err != nil {
			t.Fatal(err)
		}
		if _, err := m.LoadSnapshot(bytes.NewReader(data)); err != nil {
			t.Fatal(err)
		}
		return m
	}
	a := load()
	initial, err := a.Frame()
	if err != nil {
		t.Fatal(err)
	}
	initialPix := bytes.Clone(initial.Pix)
	s := New(a)
	s.StartRecording()
	sec := uint64(smdk2410.InstructionsPerSecond)
	t0 := a.Steps()
	for _, in := range []struct {
		dt uint64
		ev script.Event
	}{
		{sec / 10, script.Event{Kind: script.TouchDown, X: 20, Y: 10}}, // Start
		{sec / 10, script.Event{Kind: script.TouchUp}},
		{sec, script.Event{Kind: script.TouchDown, X: 50, Y: 52}}, // Calendar
		{sec/10 + 12345, script.Event{Kind: script.TouchUp}},
		{sec, script.Event{Kind: script.KeyDown, Key: "Right"}},
		{sec / 10, script.Event{Kind: script.KeyUp, Key: "Right"}},
	} {
		t0 += in.dt
		if _, err := s.Run(t0); err != nil {
			t.Fatal(err)
		}
		if err := s.Inject(in.ev); err != nil {
			t.Fatal(err)
		}
	}
	end := t0 + 2*sec
	if _, err := s.Run(end); err != nil {
		t.Fatal(err)
	}
	recStart, rec := s.StopRecording()
	var text bytes.Buffer
	if err := Format(&text, path, "", recStart, rec); err != nil {
		t.Fatal(err)
	}
	evs, err := script.Parse(&text, smdk2410.InstructionsPerSecond)
	if err != nil {
		t.Fatal(err)
	}
	b := load()
	r := New(b)
	r.Schedule(evs...)
	if _, err := r.Run(end); err != nil {
		t.Fatal(err)
	}
	fa, err := a.Frame()
	if err != nil {
		t.Fatal(err)
	}
	fb, err := b.Frame()
	if err != nil {
		t.Fatal(err)
	}
	if bytes.Equal(fa.Pix, initialPix) {
		t.Error("the taps did not change the screen (input not delivered?)")
	}
	if !bytes.Equal(fa.Pix, fb.Pix) {
		t.Error("replayed screen differs")
	}
	if !bytes.Equal(snap(t, a), snap(t, b)) {
		t.Error("replayed state differs")
	}
}
