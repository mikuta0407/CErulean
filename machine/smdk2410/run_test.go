package smdk2410

import (
	"bytes"
	"encoding/binary"
	"strings"
	"testing"

	"github.com/mikuta0407/cerulean/cpu/arm"
	"github.com/mikuta0407/cerulean/loader"
)

// idleProgram は WinCE の OAL と同じ形のアイドル待ちをするプログラム。
// MMU を有効にして VA 0（例外ベクタ）を RAM に割り当て、Timer4 の割り込み
// （約 5339 命令周期）を許可したうえで、RAM 上のフラグを
// LDR/CMP/BEQ の 3 命令ループで待つ。IRQ ハンドラがフラグを立て、ループを
// 抜けた側がカウンタ（0x30001004）を増やしてまた待つ。
//
// 物理配置: 0x30000000 ベクタ、0x30000100 リセット、0x30000164 ループ、
// 0x30000200 IRQ ハンドラ、0x30001000 フラグ、0x30004000 変換テーブル。
func idleProgram() *loader.Image {
	code := make([]byte, 0x300)
	put := func(off uint32, ws ...uint32) {
		for i, w := range ws {
			binary.LittleEndian.PutUint32(code[off+uint32(4*i):], w)
		}
	}
	put(0x00, 0xEA00003E) // B 0x100
	put(0x18, 0xEA000078) // B 0x200 (IRQ)
	put(0x100,
		0xE3A00203, // MOV r0, #0x30000000
		0xE3800901, // ORR r0, r0, #0x4000
		0xEE020F10, // MCR p15, 0, r0, c2, c0, 0   (TTB)
		0xE3E00000, // MVN r0, #0
		0xEE030F10, // MCR p15, 0, r0, c3, c0, 0   (DACR = 全マネージャ)
		0xE3A00001, // MOV r0, #1
		0xEE010F10, // MCR p15, 0, r0, c1, c0, 0   (M=1)
		0xE1A00000, // NOP
		0xE1A00000, // NOP
		0xE321F0D2, // MSR cpsr_c, #0xD2           (IRQ モード)
		0xE3A0D203, // MOV sp, #0x30000000
		0xE38DD902, // ORR sp, sp, #0x8000
		0xE321F053, // MSR cpsr_c, #0x53           (SVC、IRQ 許可)
		0xE3A04451, // MOV r4, #0x51000000         (PWM タイマー)
		0xE3A05FFA, // MOV r5, #1000
		0xE584503C, // STR r5, [r4, #0x3C]         (TCNTB4)
		0xE3A05606, // MOV r5, #0x600000           (manual update + auto reload)
		0xE5845008, // STR r5, [r4, #8]            (TCON)
		0xE3A05605, // MOV r5, #0x500000           (start + auto reload)
		0xE5845008, // STR r5, [r4, #8]
		0xE3A0444A, // MOV r4, #0x4A000000         (INTC)
		0xE3E05901, // MVN r5, #0x4000             (Timer4 = INT 14 だけ許可)
		0xE5845008, // STR r5, [r4, #8]            (INTMSK)
		0xE3A06203, // MOV r6, #0x30000000
		0xE3866A01, // ORR r6, r6, #0x1000         (フラグ)
		// loop (0x164):
		0xE5963000, // LDR r3, [r6]
		0xE3530000, // CMP r3, #0
		0x0AFFFFFC, // BEQ loop
		0xE3A03000, // MOV r3, #0
		0xE5863000, // STR r3, [r6]
		0xE2877001, // ADD r7, r7, #1
		0xE5867004, // STR r7, [r6, #4]
		0xEAFFFFF7, // B loop
	)
	put(0x200,
		0xE92D0003, // STMFD sp!, {r0, r1}
		0xE3A0044A, // MOV r0, #0x4A000000
		0xE3A01901, // MOV r1, #0x4000
		0xE5801000, // STR r1, [r0]                (SRCPND クリア)
		0xE5801010, // STR r1, [r0, #0x10]         (INTPND クリア)
		0xE3A00203, // MOV r0, #0x30000000
		0xE3800A01, // ORR r0, r0, #0x1000
		0xE3A01001, // MOV r1, #1
		0xE5801000, // STR r1, [r0]                (フラグを立てる)
		0xE8BD0003, // LDMFD sp!, {r0, r1}
		0xE25EF004, // SUBS pc, lr, #4
	)
	// 1 段目の変換テーブル（セクション、AP=11、ドメイン 0）。
	table := make([]byte, 0x4000)
	for _, pa := range []uint32{0x30000000, 0x4A000000, 0x51000000} {
		binary.LittleEndian.PutUint32(table[(pa>>20)*4:], pa|0xC12)
	}
	binary.LittleEndian.PutUint32(table[0:], 0x30000000|0xC12) // VA 0 → ベクタ
	return &loader.Image{
		Format: "bin", Start: 0x80000000, Length: 0x8000, Entry: 0x80000100,
		Segs: []loader.Segment{
			{Addr: 0x80000000, Data: code},
			{Addr: 0x80004000, Data: table},
		},
	}
}

func newIdleMachine(t *testing.T, skip bool) *Machine {
	t.Helper()
	m, err := New(nil)
	if err != nil {
		t.Fatal(err)
	}
	if err := m.LoadImage(idleProgram()); err != nil {
		t.Fatal(err)
	}
	m.SetIdleSkip(skip)
	m.Reset()
	return m
}

// 手で符号化した命令語が意図どおりか、逆アセンブラで確かめる（符号化ミスで
// テストが別のことを検査してしまうのを防ぐ）。
func TestIdleProgramEncoding(t *testing.T) {
	code := idleProgram().Segs[0].Data
	for _, c := range []struct {
		off  uint32
		want string
	}{
		{0x00, "b 0x00000100"}, {0x18, "b 0x00000200"},
		{0x164, "ldr r3, [r6]"}, {0x168, "cmp r3, #0x0"}, {0x16C, "beq 0x00000164"},
		{0x180, "b 0x00000164"}, {0x228, "subs pc, lr, #0x4"},
	} {
		w := binary.LittleEndian.Uint32(code[c.off:])
		if got := arm.Disasm(w, c.off); !strings.HasPrefix(got, c.want) {
			t.Errorf("%03X: %08X = %q, want %q", c.off, w, got, c.want)
		}
	}
}

// アイドルスキップの有無で、同じ命令数まで進めたときの全状態（スナップ
// ショットのバイト列）が一致すること。RunUntil の上限を不揃いに刻み、
// 上限・タイマー期限・割り込みの境界をまたぐ場合を含める。
func TestIdleSkipMatchesStepping(t *testing.T) {
	a := newIdleMachine(t, true)
	b := newIdleMachine(t, false)
	limit := uint64(0)
	for i, chunk := range []uint64{1, 150, 5000, 7, 12345, 3, 60000, 99991, 2, 150000} {
		limit += chunk
		if err := a.RunUntil(limit); err != nil {
			t.Fatalf("skip: %v", err)
		}
		if err := b.RunUntil(limit); err != nil {
			t.Fatalf("step: %v", err)
		}
		if a.Steps() != limit || b.Steps() != limit {
			t.Fatalf("steps = %d/%d, want %d", a.Steps(), b.Steps(), limit)
		}
		if !bytes.Equal(save(t, a), save(t, b)) {
			t.Fatalf("chunk %d (step %d): state differs between idle skip and stepping", i, limit)
		}
	}
	// 割り込みで実際にループを抜けていること（カウンタが進む）と、
	// 大半を飛ばしていること。
	ram, off, _ := a.bus.RAM(0x30001004)
	count := binary.LittleEndian.Uint32(ram[off:])
	if count < 50 {
		t.Errorf("counter = %d, want >= 50 (timer interrupts did not break the loop)", count)
	}
	if a.IdleSkipped() < limit/2 {
		t.Errorf("skipped %d of %d steps, want most of them", a.IdleSkipped(), limit)
	}
	if b.IdleSkipped() != 0 {
		t.Errorf("skip disabled but skipped %d", b.IdleSkipped())
	}
}

// 1 命令ずつ（Step）とまとめて（RunUntil）で結果が同じこと。
func TestStepMatchesRunUntil(t *testing.T) {
	a := newIdleMachine(t, true)
	b := newIdleMachine(t, true)
	const n = 40000
	for i := 0; i < n; i++ {
		if err := a.Step(); err != nil {
			t.Fatal(err)
		}
	}
	if err := b.RunUntil(n); err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(save(t, a), save(t, b)) {
		t.Fatal("Step x n and RunUntil(n) differ")
	}
}

// adcPollProgram は touch.dll と同じ形で、ADC の変換完了（ADCCON の ECFLG）を
// LDR/TST/BEQ のループで待つ（MMIO のポーリング）。変換を開始 → 完了を
// 待つ → カウンタ r7 を増やす、を繰り返す。ADCDLY=0xC000 なので 1 回の
// 変換は約 13 万命令。MMU は無効のまま（ソフト TLB は恒等変換で働く）。
// コードは PA 0x30001000 に置く: 0x30000000 だと直接マップのソフト TLB で
// ADC（0x58000000）と同じスロットになり、毎周詰め替えが起きる（状態が
// 変わるのでスキップされない。それ自体は正しい挙動）。
func adcPollProgram() *loader.Image {
	var code []byte
	for _, w := range []uint32{
		0xE3A04458, // MOV r4, #0x58000000   (ADC)
		0xE3A05903, // MOV r5, #0xC000
		0xE5845008, // STR r5, [r4, #8]      (ADCDLY)
		0xE3A05001, // loop: MOV r5, #1
		0xE5845000, // STR r5, [r4]          (ADCCON: ENABLE_START)
		0xE5943000, // wait: LDR r3, [r4]
		0xE3130902, // TST r3, #0x8000       (ECFLG)
		0x0AFFFFFC, // BEQ wait
		0xE2877001, // ADD r7, r7, #1
		0xEAFFFFF8, // B loop
	} {
		code = binary.LittleEndian.AppendUint32(code, w)
	}
	return &loader.Image{Format: "bin", Start: 0x80001000, Length: uint32(len(code)), Entry: 0x80001000,
		Segs: []loader.Segment{{Addr: 0x80001000, Data: code}}}
}

func TestADCPollProgramEncoding(t *testing.T) {
	code := adcPollProgram().Segs[0].Data
	for off, want := range map[uint32]string{
		0x14: "ldr r3, [r4]", 0x18: "tst r3, #0x8000", 0x1C: "beq 0x00000014", 0x24: "b 0x0000000C",
	} {
		w := binary.LittleEndian.Uint32(code[off:])
		if got := arm.Disasm(w, off); !strings.HasPrefix(got, want) {
			t.Errorf("%02X: %08X = %q, want %q", off, w, got, want)
		}
	}
}

// MMIO のポーリング（bus.StableReader）でも、スキップの有無で全状態が一致する。
func TestIdleSkipMMIOPollMatchesStepping(t *testing.T) {
	newM := func(skip bool) *Machine {
		m, err := New(nil)
		if err != nil {
			t.Fatal(err)
		}
		if err := m.LoadImage(adcPollProgram()); err != nil {
			t.Fatal(err)
		}
		m.SetIdleSkip(skip)
		m.Reset()
		return m
	}
	a, b := newM(true), newM(false)
	limit := uint64(0)
	for i, chunk := range []uint64{7, 100000, 31, 250000, 4, 400001} {
		limit += chunk
		if err := a.RunUntil(limit); err != nil {
			t.Fatal(err)
		}
		if err := b.RunUntil(limit); err != nil {
			t.Fatal(err)
		}
		if !bytes.Equal(save(t, a), save(t, b)) {
			t.Fatalf("chunk %d (step %d): state differs", i, limit)
		}
	}
	if r7 := a.CPU().Reg(7); r7 < 4 {
		t.Errorf("conversions completed = %d, want >= 4", r7)
	}
	if a.IdleSkipped() < limit/2 {
		t.Errorf("skipped %d of %d steps", a.IdleSkipped(), limit)
	}
}
