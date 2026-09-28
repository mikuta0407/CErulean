// goldencmp は一致確認の結果（-result の JSON Lines。項目は
// testdata/golden/README.md）を 2 つ比べる。Go 版・Rust 版のどちらの出力でも
// 読める（形式に依存しない値だけを比べる）。
//
//	go run ./tools/goldencmp <expected.jsonl> <actual.jsonl>
//
// 同じ (event, steps) の行どうしを比べ、食い違った項目を表示する。CPU 状態の
// ダンプが違うときは、どのレジスタが違うかも表示する。すべて一致すれば
// 終了コード 0、食い違いがあれば 1。
package main

import (
	"bufio"
	"bytes"
	"encoding/binary"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"os"
	"reflect"
)

// record は README の結果の 1 行。未知の項目は無視する（比較しない）。
type record struct {
	Format       int             `json:"format"`
	Event        string          `json:"event"`
	Steps        uint64          `json:"steps"`
	Stop         json.RawMessage `json:"stop"`
	CPU          string          `json:"cpu"`
	CPUSHA256    string          `json:"cpu_sha256"`
	RAMSHA256    string          `json:"ram_sha256"`
	UART1SHA256  string          `json:"uart1_sha256"`
	UART1Bytes   uint64          `json:"uart1_bytes"`
	ScreenSHA256 string          `json:"screen_sha256"`
	ScreenW      int             `json:"screen_w"`
	ScreenH      int             `json:"screen_h"`
}

func main() {
	if len(os.Args) != 3 {
		fmt.Fprintln(os.Stderr, "usage: goldencmp <expected.jsonl> <actual.jsonl>")
		os.Exit(2)
	}
	exp, err := load(os.Args[1])
	if err != nil {
		fatal(err)
	}
	act, err := load(os.Args[2])
	if err != nil {
		fatal(err)
	}
	if diffs := compare(exp, act); len(diffs) > 0 {
		for _, d := range diffs {
			fmt.Println(d)
		}
		os.Exit(1)
	}
	fmt.Printf("match (%d records)\n", len(exp))
}

func fatal(err error) {
	fmt.Fprintln(os.Stderr, "goldencmp:", err)
	os.Exit(2)
}

func load(path string) ([]record, error) {
	f, err := os.Open(path)
	if err != nil {
		return nil, err
	}
	defer f.Close()
	var recs []record
	sc := bufio.NewScanner(f)
	sc.Buffer(nil, 1<<20)
	for n := 1; sc.Scan(); n++ {
		line := bytes.TrimSpace(sc.Bytes())
		if len(line) == 0 {
			continue
		}
		var r record
		if err := json.Unmarshal(line, &r); err != nil {
			return nil, fmt.Errorf("%s:%d: %w", path, n, err)
		}
		if r.Format != 1 {
			return nil, fmt.Errorf("%s:%d: unsupported format %d", path, n, r.Format)
		}
		recs = append(recs, r)
	}
	return recs, sc.Err()
}

func key(r record) string { return fmt.Sprintf("%s@%d", r.Event, r.Steps) }

// compare は期待値の各行について、実際の出力の同じ (event, steps) の行と比べる。
func compare(exp, act []record) []string {
	var diffs []string
	byKey := map[string]record{}
	for _, r := range act {
		byKey[key(r)] = r
	}
	for _, e := range exp {
		a, ok := byKey[key(e)]
		if !ok {
			diffs = append(diffs, fmt.Sprintf("%s: missing in actual", key(e)))
			continue
		}
		delete(byKey, key(e))
		diffs = append(diffs, compareRecord(e, a)...)
	}
	for _, a := range act {
		if _, ok := byKey[key(a)]; ok {
			diffs = append(diffs, fmt.Sprintf("%s: not in expected", key(a)))
		}
	}
	return diffs
}

func compareRecord(e, a record) []string {
	var d []string
	k := key(e)
	if !jsonEqual(e.Stop, a.Stop) {
		d = append(d, fmt.Sprintf("%s: stop %s, want %s", k, a.Stop, e.Stop))
	}
	if e.CPU != a.CPU || e.CPUSHA256 != a.CPUSHA256 {
		d = append(d, fmt.Sprintf("%s: cpu differs", k))
		d = append(d, cpuDiff(k, e.CPU, a.CPU)...)
	}
	field := func(name string, want, got any) {
		if want != got {
			d = append(d, fmt.Sprintf("%s: %s = %v, want %v", k, name, got, want))
		}
	}
	field("ram_sha256", e.RAMSHA256, a.RAMSHA256)
	field("uart1_sha256", e.UART1SHA256, a.UART1SHA256)
	field("uart1_bytes", e.UART1Bytes, a.UART1Bytes)
	field("screen_sha256", e.ScreenSHA256, a.ScreenSHA256)
	field("screen_w", e.ScreenW, a.ScreenW)
	field("screen_h", e.ScreenH, a.ScreenH)
	return d
}

func jsonEqual(a, b json.RawMessage) bool {
	if len(a) == 0 || len(b) == 0 {
		return len(a) == len(b)
	}
	var x, y any
	if json.Unmarshal(a, &x) != nil || json.Unmarshal(b, &y) != nil {
		return bytes.Equal(a, b)
	}
	return reflect.DeepEqual(x, y)
}

// dumpFields は CPU 状態のダンプ（版数 1）の各語の名前（README の表の順）。
var dumpFields = func() []string {
	f := []string{"version", "steps"} // steps は u64（2 語分として下で扱う）
	for i := 0; i < 16; i++ {
		f = append(f, fmt.Sprintf("r%d", i))
	}
	for _, m := range []string{"usr", "fiq"} {
		for i := 8; i <= 14; i++ {
			f = append(f, fmt.Sprintf("%s_r%d", m, i))
		}
	}
	for _, m := range []string{"irq", "svc", "abt", "und"} {
		f = append(f, m+"_r13", m+"_r14")
	}
	f = append(f, "cpsr")
	for _, m := range []string{"fiq", "irq", "svc", "abt", "und"} {
		f = append(f, "spsr_"+m)
	}
	for _, c := range []string{"c1", "c2", "c3", "c5", "c6", "c13"} {
		f = append(f, "cp15_"+c)
	}
	return f
}()

// cpuDiff はダンプの食い違いをレジスタ名つきで並べる。
func cpuDiff(k, eh, ah string) []string {
	e, err1 := hex.DecodeString(eh)
	a, err2 := hex.DecodeString(ah)
	if err1 != nil || err2 != nil || len(e) != 212 || len(a) != 212 {
		return []string{fmt.Sprintf("%s:   (dump not comparable: len %d/%d)", k, len(e), len(a))}
	}
	var d []string
	off := 0
	for _, name := range dumpFields {
		size := 4
		if name == "steps" {
			size = 8
		}
		var ev, av uint64
		if size == 8 {
			ev, av = binary.LittleEndian.Uint64(e[off:]), binary.LittleEndian.Uint64(a[off:])
		} else {
			ev, av = uint64(binary.LittleEndian.Uint32(e[off:])), uint64(binary.LittleEndian.Uint32(a[off:]))
		}
		if ev != av {
			d = append(d, fmt.Sprintf("%s:   %-10s %08X, want %08X", k, name, av, ev))
		}
		off += size
	}
	return d
}
