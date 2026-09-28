package arm

import (
	"bytes"
	"math/rand"
	"testing"
)

// 特化した実行関数（special.go）が汎用の実行関数と同じ結果になることを、
// ランダムな命令語とランダムな状態（レジスタ・フラグ・メモリ）で確かめる。
// 命令語は特化の対象になる空間（データ処理・LDR/STR 即値・B/BL）から作り、
// 特化されなかったものは数えるだけにする。
func TestSpecializedMatchesGeneric(t *testing.T) {
	rng := rand.New(rand.NewSource(1))
	const memSize = 16 * 1024
	special := 0
	for i := 0; i < 100000; i++ {
		var word uint32
		switch rng.Intn(4) {
		case 0: // データ処理（即値）
			word = 0xE2000000 | rng.Uint32()&0x01FFFFFF
		case 1: // データ処理（レジスタ・シフトなし）
			word = 0xE0000000 | rng.Uint32()&0x01FFF00F
		case 2: // LDR/STR 即値
			word = 0xE4000000 | rng.Uint32()&0x01FFFFFF
		default: // B/BL
			word = 0xEA000000 | rng.Uint32()&0x01FFFFFF
		}
		fn := specialize(word)
		if fn == nil {
			continue
		}
		special++

		// 同じ初期状態のコアを 2 つ作る。アドレスがメモリ内に収まるよう、
		// レジスタは小さい値にする（LDR/STR のベース）。
		var regs [16]uint32
		for r := range regs {
			regs[r] = rng.Uint32() % memSize
			if rng.Intn(4) == 0 {
				regs[r] = rng.Uint32() // 演算の境界値も試す（アドレスに使われたら範囲外エラー同士で比べる）
			}
		}
		flags := PSR(rng.Uint32()) & (FlagN | FlagZ | FlagC | FlagV)
		initMem := make([]byte, memSize)
		rng.Read(initMem)

		run := func(f execFn) (*Core, *testMem, error) {
			mem := &testMem{data: bytes.Clone(initMem)}
			c := New(mem, nil)
			c.Reset(0x1000)
			c.regs = regs
			c.regs[15] = 0x1000 + 4 // 実行中は PC+4
			c.cpsr = PSR(ModeSvc) | flags
			err := f(c, word)
			return c, mem, err
		}
		a, am, aerr := run(fn)
		b, bm, berr := run(decodeFn(word))
		if (aerr == nil) != (berr == nil) {
			t.Fatalf("%08X: error mismatch: special %v, generic %v", word, aerr, berr)
		}
		if aerr != nil {
			continue // 範囲外アクセス: どちらもエラーならよい（この場合の部分的な状態は問わない）
		}
		if a.regs != b.regs || a.cpsr != b.cpsr || a.spinHint != b.spinHint {
			t.Fatalf("%08X (%s): state mismatch\n special regs=%08X cpsr=%v spin=%v\n generic regs=%08X cpsr=%v spin=%v",
				word, Disasm(word, 0x1000), a.regs, a.cpsr, a.spinHint, b.regs, b.cpsr, b.spinHint)
		}
		if !bytes.Equal(am.data, bm.data) {
			t.Fatalf("%08X (%s): memory mismatch", word, Disasm(word, 0x1000))
		}
	}
	if special < 25000 {
		t.Errorf("only %d specialized words tested", special)
	}
}
