package main

import (
	"bytes"
	"encoding/json"
	"os"
	"path/filepath"
	"testing"
)

// runResult は cmdRun を in-process で走らせ、-result の最後の行（停止時）を返す。
func runResult(t *testing.T, image string, extra ...string) []byte {
	t.Helper()
	out := filepath.Join(t.TempDir(), "r.jsonl")
	args := append([]string{"-rtc", "2006-01-02T15:04:05", "-history", "0", "-result", out}, extra...)
	cmdRun(append(args, image))
	b, err := os.ReadFile(out)
	if err != nil {
		t.Fatal(err)
	}
	lines := bytes.Split(bytes.TrimSpace(b), []byte("\n"))
	last := lines[len(lines)-1]
	var rec resultRecord
	if err := json.Unmarshal(last, &rec); err != nil || rec.Event != "stop" {
		t.Fatalf("last line %q: %v", last, err)
	}
	return last
}

// 途中でハッシュ・結果を取るために止まる点を増やしても、最終結果が変わらない
// こと（止まる点がアイドルスキップ・ブロック実行の区切りを変えても、状態は
// 1 命令ずつの場合と同じはず。docs/rust-migration-plan.md §5.3）。
func TestHashStopsDoNotChangeResult(t *testing.T) {
	cases := []struct {
		image string
		steps string
	}{
		{"../../testdata/golden/synthetic/idle.words", "1000000"},
		{"../../testdata/golden/synthetic/adc-poll.words", "1000000"},
	}
	if img := os.Getenv("CERULEAN_IMAGE"); img != "" {
		// リセットから 2 億命令（MMU 有効化・周辺機器の初期化を含む）。
		cases = append(cases, struct{ image, steps string }{img, "200000000"})
	}
	for _, c := range cases {
		t.Run(filepath.Base(c.image), func(t *testing.T) {
			hashOut := filepath.Join(t.TempDir(), "th")
			plain := runResult(t, c.image, "-max-steps", c.steps)
			// 半端な間隔で止まる点を増やす（タイマー期限・アイドルスキップの
			// 途中に当たるように）。
			hashed := runResult(t, c.image, "-max-steps", c.steps,
				"-trace-hash", "99991", "-trace-hash-ram", "700001", "-trace-hash-out", hashOut,
				"-checkpoint", "12345", "-checkpoint", "500000")
			if !bytes.Equal(plain, hashed) {
				t.Fatalf("result differs:\n plain  %s\n hashed %s", plain, hashed)
			}
			noSkip := runResult(t, c.image, "-max-steps", c.steps, "-no-idle-skip")
			if !bytes.Equal(plain, noSkip) {
				t.Fatalf("result differs without idle skip:\n %s\n %s", plain, noSkip)
			}
		})
	}
}
