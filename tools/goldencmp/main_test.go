package main

import (
	"encoding/binary"
	"encoding/hex"
	"strings"
	"testing"
)

func dump(r1 uint32) string {
	b := make([]byte, 212)
	binary.LittleEndian.PutUint32(b[0:], 1)
	binary.LittleEndian.PutUint32(b[12+4:], r1)
	return hex.EncodeToString(b)
}

func TestCompare(t *testing.T) {
	base := []record{
		{Format: 1, Event: "checkpoint", Steps: 10, CPU: dump(1), RAMSHA256: "r"},
		{Format: 1, Event: "stop", Steps: 20, Stop: []byte(`{"kind":"max-steps"}`), CPU: dump(2)},
	}
	same := []record{base[1], base[0]} // 順序は問わない
	same[0].Stop = []byte(`{ "kind" : "max-steps" }`)
	if d := compare(base, same); len(d) != 0 {
		t.Fatalf("want match, got %v", d)
	}

	diff := []record{base[0], base[1]}
	diff[0].CPU = dump(7)
	diff[0].RAMSHA256 = "x"
	diff[1].Stop = []byte(`{"kind":"quit"}`)
	got := strings.Join(compare(base, diff), "\n")
	for _, want := range []string{"checkpoint@10: cpu differs", "r1         00000007, want 00000001", "ram_sha256 = x, want r", "stop@20: stop"} {
		if !strings.Contains(got, want) {
			t.Errorf("missing %q in:\n%s", want, got)
		}
	}

	if d := compare(base, base[:1]); len(d) != 1 || !strings.Contains(d[0], "missing") {
		t.Errorf("missing record: %v", d)
	}
	if d := compare(base[:1], base); len(d) != 1 || !strings.Contains(d[0], "not in expected") {
		t.Errorf("extra record: %v", d)
	}
}
