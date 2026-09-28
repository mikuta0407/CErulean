// ihist はスナップショットから実行し、実行した ARM 命令の種類の分布を数える
// （特化・高速化の対象を選ぶための調査用）。指定の 1MB 範囲（既定は OAL の
// アイドルループの 0x800AF000 台）の命令と Thumb 命令は数えない。
// 命令語は Peek32 で読むのでソフト TLB が変わり、実行結果の再現性は問わない。
//
//	go run ./tools/ihist [-count 20000000] [-max-steps 300000000] [-skip-page 0x800AF] <snapshot>
package main

import (
	"flag"
	"fmt"
	"os"
	"sort"

	"github.com/mikuta0407/cerulean/cpu/arm"
	"github.com/mikuta0407/cerulean/machine/smdk2410"
)

func class(w uint32) string {
	cond := ""
	if w>>28 != 0xE {
		cond = "(cond)"
	}
	switch (w >> 25) & 7 {
	case 0:
		if w&0x0FFFFFF0 == 0x012FFF10 {
			return "bx"
		}
		if w&0x90 == 0x90 {
			if (w>>5)&3 == 0 {
				return "mul/swp"
			}
			return "ldrh/sb/sh"
		}
		op := (w >> 21) & 0xF
		if op >= 8 && op <= 11 && w&(1<<20) == 0 {
			return "mrs/msr"
		}
		s := ""
		if w&(1<<20) != 0 {
			s = "s"
		}
		kind := "reg-lsl0"
		if w&0x10 != 0 {
			kind = "reg-regshift"
		} else if w&0xFF0 != 0 {
			kind = "reg-immshift"
		}
		return fmt.Sprintf("dp %s%s %s%s", opn[op], s, kind, cond)
	case 1:
		op := (w >> 21) & 0xF
		s := ""
		if w&(1<<20) != 0 {
			s = "s"
		}
		return fmt.Sprintf("dp %s%s imm%s", opn[op], s, cond)
	case 2:
		return fmt.Sprintf("ldst imm L=%d B=%d P=%d W=%d%s", (w>>20)&1, (w>>22)&1, (w>>24)&1, (w>>21)&1, cond)
	case 3:
		return fmt.Sprintf("ldst reg L=%d%s", (w>>20)&1, cond)
	case 4:
		return fmt.Sprintf("ldm/stm L=%d%s", (w>>20)&1, cond)
	case 5:
		if w&(1<<24) != 0 {
			return "bl" + cond
		}
		return "b" + cond
	}
	return "cop/swi"
}

var opn = []string{"and", "eor", "sub", "rsb", "add", "adc", "sbc", "rsc", "tst", "teq", "cmp", "cmn", "orr", "mov", "bic", "mvn"}

func main() {
	count := flag.Int("count", 20_000_000, "数える命令数")
	maxSteps := flag.Uint64("max-steps", 300_000_000, "実行する最大命令数")
	skipPage := flag.Uint64("skip-page", 0x800AF, "数えない 4KB ページ番号（PC>>12。0 なら除外なし）")
	flag.Parse()
	if flag.NArg() != 1 {
		fmt.Fprintln(os.Stderr, "usage: ihist [-count n] [-max-steps n] [-skip-page p] <snapshot>")
		os.Exit(2)
	}
	if err := run(flag.Arg(0), *count, *maxSteps, uint32(*skipPage)); err != nil {
		fmt.Fprintln(os.Stderr, "ihist:", err)
		os.Exit(1)
	}
}

func run(snapPath string, count int, maxSteps uint64, skipPage uint32) error {
	m, err := smdk2410.New(nil)
	if err != nil {
		return err
	}
	f, err := os.Open(snapPath)
	if err != nil {
		return err
	}
	_, err = m.LoadSnapshot(f)
	f.Close()
	if err != nil {
		return err
	}
	c := m.CPU().(*arm.Core)
	cnt := map[string]int{}
	total := 0
	for i := uint64(0); i < maxSteps && total < count; i++ {
		if !c.CPSR().T() {
			pc := c.PC()
			if skipPage == 0 || pc>>12 != skipPage {
				w, _ := m.Peek32(pc)
				cnt[class(w)]++
				total++
			}
		}
		if err := m.Step(); err != nil {
			return err
		}
	}
	type kv struct {
		k string
		v int
	}
	var l []kv
	for k, v := range cnt {
		l = append(l, kv{k, v})
	}
	sort.Slice(l, func(i, j int) bool { return l[i].v > l[j].v || (l[i].v == l[j].v && l[i].k < l[j].k) })
	for _, x := range l[:min(40, len(l))] {
		fmt.Printf("%6.2f%% %s\n", 100*float64(x.v)/float64(max(total, 1)), x.k)
	}
	return nil
}
