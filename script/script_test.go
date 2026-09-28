package script

import (
	"reflect"
	"strings"
	"testing"
)

// テストでは 1 秒 = 1000 命令にして読みやすくする。
const sps = 1000

func TestParseDuration(t *testing.T) {
	tests := []struct {
		in      string
		sps     uint64
		want    uint64
		wantErr bool
	}{
		{"1s", sps, 1000, false},
		{"1.5s", sps, 1500, false},
		{"250ms", sps, 250, false},
		{"0.5ms", sps, 0, false}, // 端数命令は切り捨て
		{"12345i", sps, 12345, false},
		{"0s", sps, 0, false},
		// 実機構成の換算（135.2M 命令/秒）
		{"95s", 135_200_000, 12_844_000_000, false},
		{"1ms", 135_200_000, 135_200, false},
		{"0.001s", 135_200_000, 135_200, false},
		{"2.123456789s", 135_200_000, 287_091_357, false}, // 287091357.8 の切り捨て
		{"1.5i", sps, 0, true},
		{"10", sps, 0, true},
		{"s", sps, 0, true},
		{"1.s", sps, 0, true},
		{".5s", sps, 0, true},
		{"-1s", sps, 0, true},
		{"1e3ms", sps, 0, true},
		{"99999999999999999999s", sps, 0, true},
	}
	for _, tt := range tests {
		got, err := ParseDuration(tt.in, tt.sps)
		if (err != nil) != tt.wantErr {
			t.Errorf("ParseDuration(%q): err = %v, wantErr %v", tt.in, err, tt.wantErr)
			continue
		}
		if !tt.wantErr && got != tt.want {
			t.Errorf("ParseDuration(%q) = %d, want %d", tt.in, got, tt.want)
		}
	}
}

func TestParse(t *testing.T) {
	src := `
# Start メニューを開いて Calendar を起動する
@95s    tap 30 310          # 既定 100ms 押下
+2s     shot start.png
+500ms  down 120 160
+50ms   move 130 170
+50ms   up
@100s   key down Enter
+100ms  key up Enter
+1s     press SoftL 200ms
+0s     tap 1 2 1ms
@200000i snap s.snap
+1s     quit
`
	got, err := Parse(strings.NewReader(src), sps)
	if err != nil {
		t.Fatal(err)
	}
	want := []Event{
		{Step: 95000, Kind: TouchDown, X: 30, Y: 310, Line: 3},
		{Step: 95100, Kind: TouchUp, Line: 3},
		{Step: 97100, Kind: Shot, Path: "start.png", Line: 4}, // tap の終了から +2s
		{Step: 97600, Kind: TouchDown, X: 120, Y: 160, Line: 5},
		{Step: 97650, Kind: TouchMove, X: 130, Y: 170, Line: 6},
		{Step: 97700, Kind: TouchUp, Line: 7},
		{Step: 100000, Kind: KeyDown, Key: "Enter", Line: 8},
		{Step: 100100, Kind: KeyUp, Key: "Enter", Line: 9},
		{Step: 101100, Kind: KeyDown, Key: "SoftL", Line: 10},
		{Step: 101300, Kind: KeyUp, Key: "SoftL", Line: 10},
		{Step: 101300, Kind: TouchDown, X: 1, Y: 2, Line: 11},
		{Step: 101301, Kind: TouchUp, Line: 11},
		{Step: 200000, Kind: Snap, Path: "s.snap", Line: 12},
		{Step: 201000, Kind: Quit, Line: 13},
	}
	if !reflect.DeepEqual(got, want) {
		t.Errorf("Parse mismatch:\n got %+v\nwant %+v", got, want)
	}
}

func TestParseErrors(t *testing.T) {
	tests := []struct{ src, want string }{
		{"95s tap 1 2", "must start with @"},
		{"@1s", "expected <time> <command>"},
		{"@2s up\n@1s up", "line 2: time @1s is before"},
		{"@1s tap 1", "usage: tap"},
		{"@1s tap -1 2", "bad x"},
		{"@1s tap 1 2 0ms", "hold time must be > 0"},
		{"@1s key press A", "usage: key"},
		{"@1s jump", `unknown command "jump"`},
		{"@1s shot", "usage: shot"},
		{"@1s up now", "usage: up"},
		{"@1s press", "usage: press"},
		{"@1s tap 1 2 5", "needs a unit"},
		// tap は押下時間の後が終了なので、同じ時刻への絶対指定は後戻りになる
		{"@1s tap 1 2\n@1s up", "before the previous command"},
	}
	for _, tt := range tests {
		_, err := Parse(strings.NewReader(tt.src), sps)
		if err == nil || !strings.Contains(err.Error(), tt.want) {
			t.Errorf("Parse(%q): err = %v, want containing %q", tt.src, err, tt.want)
		}
	}
}
