package main

import (
	"bytes"
	"context"
	"encoding/binary"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"os"
	"strconv"
	"strings"
	"testing"

	"github.com/mikuta0407/cerulean/emu"
	"github.com/mikuta0407/cerulean/loader"
	"github.com/mikuta0407/cerulean/machine"
	"github.com/mikuta0407/cerulean/machine/smdk2410"
	"github.com/mikuta0407/cerulean/script"
)

// counterMachine は LCD を 240x320 16bpp（FB = PA 0x33F00000）に設定し、
// 左上の画素にカウンタを書き続けるマシン。画面が命令数に応じて変わるので、
// 記録を再生した画面の一致で「同じ命令数で止まったか」まで確かめられる。
func counterMachine(t *testing.T) *smdk2410.Machine {
	t.Helper()
	m, err := smdk2410.New(nil)
	if err != nil {
		t.Fatal(err)
	}
	var prog []byte
	for _, w := range []uint32{
		0xE3A01433, // MOV r1, #0x33000000
		0xE381160F, // ORR r1, r1, #0xF00000
		0xE2800001, // loop: ADD r0, r0, #1
		0xE1C100B0, // STRH r0, [r1]
		0xEAFFFFFC, // B loop
	} {
		prog = binary.LittleEndian.AppendUint32(prog, w)
	}
	img := &loader.Image{Format: "bin", Start: 0x80000000, Length: uint32(len(prog)), Entry: 0x80000000,
		Segs: []loader.Segment{{Addr: 0x80000000, Data: prog}}}
	if err := m.LoadImage(img); err != nil {
		t.Fatal(err)
	}
	m.Reset()
	const fb = 0x33F00000
	for off, v := range map[uint32]uint32{
		0x00: 3<<5 | 0xC<<1 | 1, // LCDCON1: TFT 16bpp ENVID=1
		0x04: 319 << 14,         // LCDCON2: 320 行
		0x08: 239 << 8,          // LCDCON3: 240 列
		0x10: 0xB01,             // LCDCON5: FRM565・HWSWP
		0x14: (fb>>22)<<21 | (fb>>1)&0x1FFFFF,
		0x1C: 240,
	} {
		if err := m.Bus().Write32(0x4D000000+off, v); err != nil {
			t.Fatal(err)
		}
	}
	return m
}

type testServer struct {
	*httptest.Server
	s      *server
	cancel context.CancelFunc
}

func startTestServer(t *testing.T) *testServer {
	t.Helper()
	s := newServer(counterMachine(t), "img", t.TempDir())
	ctx, cancel := context.WithCancel(context.Background())
	go s.loop(ctx)
	ts := &testServer{Server: httptest.NewServer(s.handler()), s: s, cancel: cancel}
	t.Cleanup(func() { ts.Close(); cancel() })
	return ts
}

func (ts *testServer) post(t *testing.T, path string, body any) (int, map[string]any) {
	t.Helper()
	b, _ := json.Marshal(body)
	resp, err := http.Post(ts.URL+path, "application/json", bytes.NewReader(b))
	if err != nil {
		t.Fatal(err)
	}
	defer resp.Body.Close()
	var v map[string]any
	_ = json.NewDecoder(resp.Body).Decode(&v)
	return resp.StatusCode, v
}

func TestServeFrameLongPoll(t *testing.T) {
	ts := startTestServer(t)
	get := func(since uint64) (int, uint64, int) {
		resp, err := http.Get(ts.URL + "/frame?since=" + strconv.FormatUint(since, 10))
		if err != nil {
			t.Fatal(err)
		}
		defer resp.Body.Close()
		var buf bytes.Buffer
		_, _ = buf.ReadFrom(resp.Body)
		seq, _ := strconv.ParseUint(resp.Header.Get("X-Frame-Seq"), 10, 64)
		return resp.StatusCode, seq, buf.Len()
	}
	code, seq, n := get(0)
	if code != 200 || seq == 0 || n != 240*320*4 {
		t.Fatalf("first frame: status %d seq %d len %d", code, seq, n)
	}
	// 画面は動き続けるので、次の要求はより新しい画面を返す。
	code, seq2, _ := get(seq)
	if code != 200 || seq2 <= seq {
		t.Fatalf("next frame: status %d seq %d (prev %d)", code, seq2, seq)
	}
	// ずっと先の番号を待つと、タイムアウトで 204。
	if code, _, _ := get(1 << 60); code != http.StatusNoContent {
		t.Errorf("waiting for a future frame: status %d, want 204", code)
	}
}

// UI の操作を記録 → 書き出したスクリプトを run と同じ経路で再生し、停止時の
// 画面（expected）と再生の画面（replay）が一致すること。
func TestServeRecordReplay(t *testing.T) {
	ts := startTestServer(t)
	code, start := ts.post(t, "/record/start", nil)
	if code != 200 {
		t.Fatalf("record/start: %d %v", code, start)
	}
	for _, in := range []inputReq{
		{T: "down", X: 20, Y: 10}, {T: "move", X: 30, Y: 40}, {T: "up"},
		{T: "keydown", Key: "Right"}, {T: "keyup", Key: "Right"},
	} {
		if code, v := ts.post(t, "/input", in); code != 200 {
			t.Fatalf("input %+v: %d %v", in, code, v)
		}
	}
	code, res := ts.post(t, "/record/stop", nil)
	if code != 200 {
		t.Fatalf("record/stop: %d %v", code, res)
	}
	if res["events"].(float64) != 5 {
		t.Errorf("recorded %v events, want 5", res["events"])
	}

	// 再生（cmd run と同じ: スナップショットから、スクリプトのイベントを
	// applyEvent で適用）。
	m, err := smdk2410.New(nil)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := startMachine(m, "", res["snapshot"].(string), "", 0); err != nil {
		t.Fatal(err)
	}
	f, err := os.Open(res["script"].(string))
	if err != nil {
		t.Fatal(err)
	}
	evs, err := script.Parse(f, smdk2410.InstructionsPerSecond)
	f.Close()
	if err != nil {
		t.Fatal(err)
	}
	sess := emu.New(m)
	sess.Apply = func(_ machine.Machine, ev script.Event) (bool, error) { return applyEvent(m, ev, "img") }
	sess.Schedule(evs...)
	quit, err := sess.Run(^uint64(0))
	if err != nil || !quit {
		t.Fatalf("replay: quit=%v err=%v", quit, err)
	}
	want, err := os.ReadFile(res["expected"].(string))
	if err != nil {
		t.Fatal(err)
	}
	got, err := os.ReadFile(res["replay"].(string))
	if err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(got, want) {
		t.Error("replayed screen differs from the screen at the end of recording")
	}
}

func TestServeRejects(t *testing.T) {
	ts := startTestServer(t)
	if code, _ := ts.post(t, "/input", inputReq{T: "keydown", Key: "NoSuchKey"}); code != 400 {
		t.Errorf("unknown key: %d, want 400", code)
	}
	if code, _ := ts.post(t, "/input", inputReq{T: "down", X: 240, Y: 0}); code != 400 {
		t.Errorf("out of screen: %d, want 400", code)
	}
	if code, _ := ts.post(t, "/record/stop", nil); code != 400 {
		t.Errorf("stop without start: %d, want 400", code)
	}
	req, _ := http.NewRequest("POST", ts.URL+"/snapshot", strings.NewReader("{}"))
	req.Header.Set("Origin", "http://evil.example")
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		t.Fatal(err)
	}
	resp.Body.Close()
	if resp.StatusCode != http.StatusForbidden {
		t.Errorf("cross-origin POST: %d, want 403", resp.StatusCode)
	}
}

func TestServeControl(t *testing.T) {
	ts := startTestServer(t)
	if code, v := ts.post(t, "/control", controlReq{Op: "pause"}); code != 200 || v["paused"] != true {
		t.Fatalf("pause: %d %v", code, v)
	}
	// 一時停止中は命令数が進まない（入力・保存は受け付ける）。
	_, a := ts.post(t, "/input", inputReq{T: "keydown", Key: "A"})
	_, b := ts.post(t, "/input", inputReq{T: "keyup", Key: "A"})
	if a["step"] != b["step"] {
		t.Errorf("steps advanced while paused: %v -> %v", a["step"], b["step"])
	}
	if code, _ := ts.post(t, "/snapshot", nil); code != 200 {
		t.Errorf("snapshot while paused: %d", code)
	}
	if code, v := ts.post(t, "/control", controlReq{Op: "speed", Speed: 0}); code != 200 || v["speed"] != 0.0 {
		t.Errorf("speed: %d %v", code, v)
	}
	if code, _ := ts.post(t, "/control", controlReq{Op: "bogus"}); code != 400 {
		t.Errorf("bogus op: %d", code)
	}
}
