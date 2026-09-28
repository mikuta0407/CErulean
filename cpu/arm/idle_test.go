package arm

import "testing"

// probeMem は testMem に cpu.Prober を足したもの。noProbe の範囲は
// 「状態を変えずには読めない」（TLB ミスや MMIO 相当）とする。
type probeMem struct {
	testMem
	noProbeLo, noProbeHi uint32
}

func (m *probeMem) Probe32(a uint32, fetch bool) (uint32, bool) {
	if a&3 != 0 || (a >= m.noProbeLo && a < m.noProbeHi) {
		return 0, false
	}
	v, err := m.Read32(a)
	return v, err == nil
}

// PollLoop の判定表。ループは testPC に置き、r6 がフラグ 0x2000 を指す。
// 各ケースはループを 3 命令分実行して先頭に戻った状態で PollLoop を呼ぶ。
func TestPollLoop(t *testing.T) {
	const flag = 0x2000
	for _, c := range []struct {
		name  string
		words [3]uint32
		setup func(m *probeMem)
		want  bool
	}{
		{"ldr/cmp/beq", [3]uint32{0xE5963000, 0xE3530000, 0x0AFFFFFC}, nil, true},
		{"ldrb/tst/beq", [3]uint32{0xE5D63000, 0xE3130001, 0x0AFFFFFC}, nil, true},
		{"ldr with offset/cmp reg", [3]uint32{0xE5163004, 0xE1530007, 0x1AFFFFFC}, nil, true},
		{"writeback", [3]uint32{0xE5B63000, 0xE3530000, 0x0AFFFFFC}, nil, false},
		{"rd == rn", [3]uint32{0xE5966000, 0xE3560000, 0x0AFFFFFC}, nil, false},
		{"not a compare (sub)", [3]uint32{0xE5963000, 0xE2533000, 0x0AFFFFFC}, nil, false},
		{"branch with link", [3]uint32{0xE5963000, 0xE3530000, 0x0BFFFFFC}, nil, false},
		{"load not probeable", [3]uint32{0xE5963000, 0xE3530000, 0x0AFFFFFC},
			func(m *probeMem) { m.noProbeLo, m.noProbeHi = flag-4, flag+4 }, false},
		{"code not probeable", [3]uint32{0xE5963000, 0xE3530000, 0x0AFFFFFC},
			func(m *probeMem) { m.noProbeLo, m.noProbeHi = testPC, testPC+4 }, false},
		{"memory changed since load", [3]uint32{0xE5963000, 0xE3530000, 0x0AFFFFFC},
			func(m *probeMem) { _ = m.Write32(flag, 1) }, false},
	} {
		t.Run(c.name, func(t *testing.T) {
			mem := &probeMem{testMem: testMem{data: make([]byte, 64*1024)}}
			core := New(mem, nil)
			core.Reset(testPC)
			core.cpsr &^= FlagI
			core.regs[6] = flag
			core.regs[7] = 1 // cmp r3, r7 が不成立（r3=0）で回り続けるように
			if c.words[0] == 0xE5163004 {
				core.regs[6] = flag + 4 // [r6, #-4]
			}
			for i, w := range c.words {
				_ = mem.Write32(testPC+uint32(4*i), w)
			}
			for i := 0; i < 3; i++ {
				if err := core.Step(); err != nil {
					t.Fatal(err)
				}
			}
			if core.PC() != testPC {
				t.Fatalf("loop did not return to its head: PC=%08X", core.PC())
			}
			if !core.TakeSpinHint() && c.words[2]&0x01000000 == 0 {
				t.Error("branch back to the loop head did not set the spin hint")
			}
			if c.setup != nil {
				c.setup(mem)
			}
			_, ok := core.PollLoop()
			if ok != c.want {
				t.Errorf("PollLoop ok = %v, want %v", ok, c.want)
			}
		})
	}
}

// 受け付け可能な割り込みが保留中なら、ループは次の Step で抜けるので不可。
func TestPollLoopInterruptPending(t *testing.T) {
	mem := &probeMem{testMem: testMem{data: make([]byte, 64*1024)}}
	core := New(mem, nil)
	core.Reset(testPC)
	core.regs[6] = 0x2000
	for i, w := range []uint32{0xE5963000, 0xE3530000, 0x0AFFFFFC} {
		_ = mem.Write32(testPC+uint32(4*i), w)
	}
	core.SetIRQ(true) // リセット直後は I=1 なのでまだ受け付けない
	if _, ok := core.PollLoop(); !ok {
		t.Fatal("masked IRQ should not prevent the idle skip")
	}
	core.cpsr &^= FlagI
	if _, ok := core.PollLoop(); ok {
		t.Fatal("PollLoop must fail while an enabled IRQ is pending")
	}
}

// スキップした区間がループの PC 列として履歴に補われること。
func TestSkipPollLoopHistory(t *testing.T) {
	core, _ := newTestCore()
	core.SetHistory(5)
	core.SkipPollLoop(9) // 3 周分
	want := []uint32{testPC + 8, testPC, testPC + 4, testPC + 8}
	h := core.History()
	if len(h) != 5 {
		t.Fatalf("len = %d", len(h))
	}
	for i, pc := range append([]uint32{testPC + 4}, want...) {
		if h[i].PC != pc {
			t.Errorf("hist[%d] = %08X, want %08X", i, h[i].PC, pc)
		}
	}
	if core.PC() != testPC {
		t.Errorf("PC changed: %08X", core.PC())
	}
}
