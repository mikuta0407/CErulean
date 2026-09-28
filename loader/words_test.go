package loader

import (
	"bytes"
	"strings"
	"testing"
)

func TestLoadWords(t *testing.T) {
	src := `# comment
entry 0x80000100
org 80000000
EA00003E   # B 0x100
1
org 0x80000100
DEADBEEF
`
	img, err := LoadWords(strings.NewReader(src))
	if err != nil {
		t.Fatal(err)
	}
	if img.Format != "words" || img.Entry != 0x80000100 || img.Start != 0x80000000 || img.Length != 0x104 {
		t.Fatalf("got format=%s entry=%08X start=%08X len=%X", img.Format, img.Entry, img.Start, img.Length)
	}
	if len(img.Segs) != 2 || img.Segs[0].Addr != 0x80000000 || img.Segs[1].Addr != 0x80000100 {
		t.Fatalf("segs = %+v", img.Segs)
	}
	if !bytes.Equal(img.Segs[0].Data, []byte{0x3E, 0x00, 0x00, 0xEA, 1, 0, 0, 0}) ||
		!bytes.Equal(img.Segs[1].Data, []byte{0xEF, 0xBE, 0xAD, 0xDE}) {
		t.Fatalf("data = % X / % X", img.Segs[0].Data, img.Segs[1].Data)
	}
}

func TestLoadWordsErrors(t *testing.T) {
	for _, src := range []string{
		"org 0\n1\n",                    // entry なし
		"entry 0\n",                     // 語なし
		"entry 0\n1\n",                  // org の前の語
		"entry 0\nentry 0\norg 0\n1\n",  // entry が 2 回
		"entry 0\norg 2\n1\n",           // 非アライン
		"entry 0\norg 0\n123456789\n",   // 9 桁
		"entry 0\norg 0\nXYZ\n",         // 16 進でない
		"entry 0\norg 0\n1 2\n",         // 1 行 2 語
		"entry 0\norg FFFFFFFC\n1\n2\n", // 32 ビットを越える
		"entry\norg 0\n1\n",             // 引数なし
	} {
		if _, err := LoadWords(strings.NewReader(src)); err == nil {
			t.Errorf("%q: want error", src)
		}
	}
}
