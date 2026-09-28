package mmu

import (
	"testing"

	"github.com/mikuta0407/cerulean/bus"
)

// MMU 有効化直後のフェッチ猶予（パイプライン近似）のテスト。
// 実イメージのブートコードが依存するイディオム:
//
//	mcr p15, 0, r1, c1, c0, 0  ; MMU 有効化
//	mov pc, r0                 ; ← 物理のままフェッチ済みで実行される
//	（分岐先からは変換が効く）
func newGraceMMU(t *testing.T) (*MMU, *bus.Bus) {
	t.Helper()
	b := bus.New()
	if err := b.MapRAM("ram", 0, 2<<20); err != nil {
		t.Fatal(err)
	}
	m := New(b)
	setCP15(t, m, 2, testTTB)
	setCP15(t, m, 3, 0x1)
	// 物理 0x100/0x104 に目印（ページテーブルは全フォルトのまま）
	for _, a := range []uint32{0x100, 0x104} {
		if err := b.Write32(a, 0xAAAAAAAA); err != nil {
			t.Fatal(err)
		}
	}
	setCP15(t, m, 1, ctrlM) // 有効化 → 連続 2 フェッチの猶予
	return m, b
}

func TestFetchGraceSequential(t *testing.T) {
	m, _ := newGraceMMU(t)
	// 連続した 2 命令は物理のまま
	for _, a := range []uint32{0x100, 0x104} {
		if v, err := m.Fetch32(a); err != nil || v != 0xAAAAAAAA {
			t.Fatalf("grace fetch at %X: %08X, %v", a, v, err)
		}
	}
	// 3 命令目からは変換される（未マップなので変換フォルト）
	_, err := m.Fetch32(0x108)
	wantAbort(t, err, fsTransSect, 0, false)
}

func TestFetchGraceEndsOnBranch(t *testing.T) {
	m, _ := newGraceMMU(t)
	if _, err := m.Fetch32(0x100); err != nil {
		t.Fatal(err)
	}
	// 非連続アドレス（分岐先）はパイプラインフラッシュ相当で新状態
	_, err := m.Fetch32(0x200)
	wantAbort(t, err, fsTransSect, 0, false)
}

func TestDataReadIgnoresGrace(t *testing.T) {
	m, _ := newGraceMMU(t)
	// データリードは猶予に関係なく即時に新状態
	_, err := m.Read32(0x100)
	wantAbort(t, err, fsTransSect, 0, false)
}
