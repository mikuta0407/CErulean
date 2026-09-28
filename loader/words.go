package loader

import (
	"bufio"
	"encoding/binary"
	"fmt"
	"io"
	"strconv"
	"strings"
)

// LoadWords は命令語のテキスト（拡張子 .words）を読み込む。合成プログラム
// （テスト・一致確認の基準シナリオ synthetic-*）を Go と Rust の両方から
// 同じファイルで使うための形式で、実イメージの形式ではない。
//
// 書式（1 行 1 項目、"#" 以降はコメント、16 進は 0x なしでも可）:
//
//	entry <アドレス>   エントリポイント（CE 仮想アドレス。1 回だけ）
//	org <アドレス>     以降の語を置くアドレス（新しいセグメントを始める。4 の倍数）
//	<語>               32 ビットの語（8 桁以下の 16 進）。LE で置き、アドレスを 4 進める
//
// 置かれなかった場所は RAM の初期値（0）のまま。
func LoadWords(r io.Reader) (*Image, error) {
	img := &Image{Format: "words"}
	var (
		cur      *Segment
		addr     uint64 // 次の語を置くアドレス（32 ビットを越えたら誤り）
		hasEntry bool
		lo, hi   uint64 // 置いた範囲 [lo, hi)
		lineNo   int
	)
	lo = 1 << 32
	sc := bufio.NewScanner(r)
	for sc.Scan() {
		lineNo++
		line, _, _ := strings.Cut(sc.Text(), "#")
		f := strings.Fields(line)
		if len(f) == 0 {
			continue
		}
		bad := func(format string, a ...any) error {
			return fmt.Errorf("loader: words line %d: %s", lineNo, fmt.Sprintf(format, a...))
		}
		switch f[0] {
		case "entry", "org":
			if len(f) != 2 {
				return nil, bad("%s wants one address", f[0])
			}
			v, err := parseHex32(f[1])
			if err != nil {
				return nil, bad("%v", err)
			}
			if f[0] == "entry" {
				if hasEntry {
					return nil, bad("duplicate entry")
				}
				img.Entry, hasEntry = v, true
				continue
			}
			if v%4 != 0 {
				return nil, bad("org %08X is not word aligned", v)
			}
			img.Segs = append(img.Segs, Segment{Addr: v})
			cur, addr = &img.Segs[len(img.Segs)-1], uint64(v)
		default:
			if cur == nil {
				return nil, bad("word before the first org")
			}
			if len(f) != 1 {
				return nil, bad("one word per line")
			}
			w, err := parseHex32(f[0])
			if err != nil {
				return nil, bad("%v", err)
			}
			if addr+4 > 1<<32 {
				return nil, bad("address overflows 32 bits")
			}
			cur.Data = binary.LittleEndian.AppendUint32(cur.Data, w)
			lo, hi = min(lo, addr), max(hi, addr+4)
			addr += 4
		}
	}
	if err := sc.Err(); err != nil {
		return nil, err
	}
	if !hasEntry {
		return nil, fmt.Errorf("loader: words: no entry")
	}
	if hi == 0 {
		return nil, fmt.Errorf("loader: words: no words")
	}
	img.Start, img.Length = uint32(lo), uint32(hi-lo)
	return img, nil
}

func parseHex32(s string) (uint32, error) {
	s = strings.TrimPrefix(strings.TrimPrefix(s, "0x"), "0X")
	if len(s) == 0 || len(s) > 8 {
		return 0, fmt.Errorf("bad hex value %q", s)
	}
	v, err := strconv.ParseUint(s, 16, 32)
	if err != nil {
		return 0, fmt.Errorf("bad hex value %q", s)
	}
	return uint32(v), nil
}
