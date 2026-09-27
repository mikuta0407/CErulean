package s3c2410

import "testing"

func TestStubReadWrite(t *testing.T) {
	s := NewStub("test", map[uint32]uint32{0x10: 0xDEADBEEF})

	tests := []struct {
		name string
		run  func()
		off  uint32
		size int
		want uint32
	}{
		{"初期値ワード", nil, 0x10, 4, 0xDEADBEEF},
		{"初期値バイト0", nil, 0x10, 1, 0xEF},
		{"初期値バイト3", nil, 0x13, 1, 0xDE},
		{"初期値ハーフ下位", nil, 0x10, 2, 0xBEEF},
		{"初期値ハーフ上位", nil, 0x12, 2, 0xDEAD},
		{"未書き込みは0", nil, 0x20, 4, 0},
		{"ワード書き込み", func() { s.Write(0x00, 4, 0x11223344) }, 0x00, 4, 0x11223344},
		{"バイト書き込みは該当バイトのみ", func() { s.Write(0x01, 1, 0xAA) }, 0x00, 4, 0x1122AA44},
		{"ハーフ書き込みは該当ハーフのみ", func() { s.Write(0x02, 2, 0x5566) }, 0x00, 4, 0x5566AA44},
	}
	for _, tt := range tests {
		if tt.run != nil {
			tt.run()
		}
		if got := s.Read(tt.off, tt.size); got != tt.want {
			t.Errorf("%s: Read(%#x, %d) = %08X, want %08X", tt.name, tt.off, tt.size, got, tt.want)
		}
	}
}
