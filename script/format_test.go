package script

import (
	"bytes"
	"reflect"
	"strings"
	"testing"
)

// Format → Parse の往復でイベント列（行番号以外）が一致すること。
func TestFormatRoundTrip(t *testing.T) {
	in := []Event{
		{Step: 3600000000, Kind: TouchDown, X: 20, Y: 10},
		{Step: 3600000001, Kind: TouchMove, X: 21, Y: 11},
		{Step: 3613520000, Kind: TouchUp},
		{Step: 3613520000, Kind: KeyDown, Key: "Right"},
		{Step: 3700000000, Kind: KeyUp, Key: "Right"},
		{Step: 3700000000, Kind: Shot, Path: "out/a.png"},
		{Step: 3700000000, Kind: Snap, Path: "a.snap"},
		{Step: 3700000001, Kind: Quit},
	}
	var buf bytes.Buffer
	if err := Format(&buf, []string{"recorded by test", "start: x.snap"}, in); err != nil {
		t.Fatal(err)
	}
	if !strings.HasPrefix(buf.String(), "# recorded by test\n# start: x.snap\n@3600000000i down 20 10\n") {
		t.Errorf("unexpected text:\n%s", buf.String())
	}
	out, err := Parse(&buf, 135200000)
	if err != nil {
		t.Fatal(err)
	}
	for i := range out {
		out[i].Line = 0
	}
	if !reflect.DeepEqual(in, out) {
		t.Errorf("round trip mismatch:\n in  %+v\n out %+v", in, out)
	}
}

func TestFormatErrors(t *testing.T) {
	for _, c := range []struct {
		name string
		ev   []Event
	}{
		{"out of order", []Event{{Step: 2, Kind: TouchUp}, {Step: 1, Kind: TouchUp}}},
		{"space in path", []Event{{Kind: Shot, Path: "a b.png"}}},
		{"hash in path", []Event{{Kind: Snap, Path: "a#b"}}},
		{"empty key", []Event{{Kind: KeyDown}}},
		{"negative coordinate", []Event{{Kind: TouchDown, X: -1}}},
	} {
		if err := Format(&bytes.Buffer{}, nil, c.ev); err == nil {
			t.Errorf("%s: want error", c.name)
		}
	}
}
