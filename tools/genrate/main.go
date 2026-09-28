// genrate はリセットから指定の命令数まで実行し、MMU の変換世代の増加頻度と、
// コードページの印付け・書き込み検出の回数を表示する（デコードキャッシュの
// 性能の調査用。世代が上がるたびに CPU は実行中ページの記憶を捨てる）。
//
//	go run ./tools/genrate [-rtc 2006-01-02T15:04:05] [-steps 600000000] <image>
package main

import (
	"flag"
	"fmt"
	"os"
	"time"

	"github.com/mikuta0407/cerulean/loader"
	"github.com/mikuta0407/cerulean/machine/smdk2410"
)

func main() {
	rtc := flag.String("rtc", "2006-01-02T15:04:05", "RTC の初期時刻")
	steps := flag.Uint64("steps", 600_000_000, "実行する命令数")
	flag.Parse()
	if flag.NArg() != 1 || *steps == 0 {
		fmt.Fprintln(os.Stderr, "usage: genrate [-rtc time] [-steps n] <image>")
		os.Exit(2)
	}
	if err := run(flag.Arg(0), *rtc, *steps); err != nil {
		fmt.Fprintln(os.Stderr, "genrate:", err)
		os.Exit(1)
	}
}

func run(path, rtc string, n uint64) error {
	m, err := smdk2410.New(nil)
	if err != nil {
		return err
	}
	img, err := loader.Load(path, 0x30000000)
	if err != nil {
		return err
	}
	if err := m.LoadImage(img); err != nil {
		return err
	}
	t, err := time.Parse("2006-01-02T15:04:05", rtc)
	if err != nil {
		return fmt.Errorf("-rtc: %w", err)
	}
	m.SetRTC(t)
	m.Reset()
	if err := m.RunUntil(n); err != nil {
		return err
	}
	marks, writes := m.MMU().CodeStats()
	gen := *m.MMU().CodeGen()
	fmt.Printf("gen=%d (%.1f per 1000 instr) codeMarks=%d codeWrites=%d\n",
		gen, float64(gen)/float64(n)*1000, marks, writes)
	return nil
}
