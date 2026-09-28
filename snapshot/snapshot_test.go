package snapshot

import (
	"bytes"
	"strings"
	"testing"
)

// sample は全エンコード型を使うテスト用コンポーネント。
type sample struct {
	ver   uint16
	a     uint8
	b     uint16
	c     uint32
	d     uint64
	e     int64
	f     bool
	g     []byte
	h     string
	i     [3]uint32
	extra bool // true なら 1 バイト余分に書く（読み手との食い違いの再現）
}

func (s *sample) StateVersion() uint16 { return s.ver }
func (s *sample) SaveState(e *Encoder) {
	e.U8(s.a)
	e.U16(s.b)
	e.U32(s.c)
	e.U64(s.d)
	e.I64(s.e)
	e.Bool(s.f)
	e.Bytes(s.g)
	e.String(s.h)
	e.U32s(s.i[:])
	if s.extra {
		e.U8(0xEE)
	}
}
func (s *sample) LoadState(d *Decoder) {
	if !d.CheckVersion(1) {
		return
	}
	s.a = d.U8()
	s.b = d.U16()
	s.c = d.U32()
	s.d = d.U64()
	s.e = d.I64()
	s.f = d.Bool()
	s.g = d.Bytes()
	s.h = d.String()
	d.U32sInto(s.i[:])
}

func save(t *testing.T, chunks map[string]Stateful, order []string) []byte {
	t.Helper()
	var buf bytes.Buffer
	w, err := NewWriter(&buf, "mach", "img-1")
	if err != nil {
		t.Fatal(err)
	}
	for _, n := range order {
		if err := w.Chunk(n, chunks[n]); err != nil {
			t.Fatal(err)
		}
	}
	if err := w.Close(); err != nil {
		t.Fatal(err)
	}
	return buf.Bytes()
}

func TestRoundTrip(t *testing.T) {
	src := &sample{ver: 1, a: 0x12, b: 0x3456, c: 0x789ABCDE, d: 1 << 60, e: -5, f: true,
		g: []byte{1, 2, 3}, h: "hello", i: [3]uint32{7, 8, 9}}
	data := save(t, map[string]Stateful{"x": src}, []string{"x"})

	r, err := NewReader(bytes.NewReader(data))
	if err != nil {
		t.Fatal(err)
	}
	if r.Header != (Header{Machine: "mach", ImageID: "img-1"}) {
		t.Errorf("header = %+v", r.Header)
	}
	dst := &sample{ver: 1}
	if err := r.Chunk("x", dst); err != nil {
		t.Fatal(err)
	}
	if err := r.Close(); err != nil {
		t.Fatal(err)
	}
	if string(dst.g) != string(src.g) || dst.h != src.h || dst.a != src.a || dst.b != src.b ||
		dst.c != src.c || dst.d != src.d || dst.e != src.e || dst.f != src.f || dst.i != src.i {
		t.Errorf("round trip mismatch:\n got %+v\nwant %+v", dst, src)
	}
}

func TestErrors(t *testing.T) {
	good := func() *sample { return &sample{ver: 1, h: "x"} }
	tests := []struct {
		name    string
		data    func() []byte
		read    []string
		wantErr string
	}{
		{"bad magic", func() []byte { return []byte("NOTASNAP\x01\x00\x00\x00") }, nil, "bad magic"},
		{"wrong chunk name", func() []byte {
			return save(t, map[string]Stateful{"x": good()}, []string{"x"})
		}, []string{"y"}, `expected chunk "y", found "x"`},
		{"unsupported version", func() []byte {
			return save(t, map[string]Stateful{"x": &sample{ver: 2}}, []string{"x"})
		}, []string{"x"}, "unsupported state version 2"},
		{"writer wrote more than reader read", func() []byte {
			s := good()
			s.extra = true
			return save(t, map[string]Stateful{"x": s}, []string{"x"})
		}, []string{"x"}, "writer/reader mismatch"},
		{"extra chunk", func() []byte {
			return save(t, map[string]Stateful{"x": good(), "y": good()}, []string{"x", "y"})
		}, []string{"x"}, `unexpected extra chunk "y"`},
		{"truncated", func() []byte {
			d := save(t, map[string]Stateful{"x": good()}, []string{"x"})
			return d[:len(d)-4]
		}, []string{"x"}, "unexpected EOF"},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			var err error
			r, err := NewReader(bytes.NewReader(tt.data()))
			if err == nil {
				for _, n := range tt.read {
					if err = r.Chunk(n, &sample{}); err != nil {
						break
					}
				}
				if err == nil {
					err = r.Close()
				}
			}
			if err == nil || !strings.Contains(err.Error(), tt.wantErr) {
				t.Errorf("err = %v, want containing %q", err, tt.wantErr)
			}
		})
	}
}

func TestBytesIntoLengthMismatch(t *testing.T) {
	var buf bytes.Buffer
	e := &Encoder{w: &buf}
	e.Bytes(make([]byte, 4))
	d := &Decoder{r: &buf}
	d.BytesInto(make([]byte, 8))
	if d.Err() == nil {
		t.Error("BytesInto accepted a length mismatch")
	}
}
