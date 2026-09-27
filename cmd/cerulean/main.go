// cerulean は PC 開発用の CLI フロントエンド。
// コア（loader/cpu/bus/machine）は UI 非依存なので、ここにだけ
// 標準入出力・フラグ処理などを置く。
package main

import (
	"errors"
	"flag"
	"fmt"
	"os"

	"github.com/mikuta0407/cerulean/bus"
	"github.com/mikuta0407/cerulean/cpu/arm"
	"github.com/mikuta0407/cerulean/loader"
	"github.com/mikuta0407/cerulean/machine/smdk2410"
)

func usage() {
	fmt.Fprintf(os.Stderr, `Usage:
  cerulean info <image>                 イメージの情報を表示する
  cerulean run [flags] <image>          イメージを実行する

run flags:
  -machine name    マシン構成 (default "smdk2410")
  -max-steps n     最大実行命令数 (0 = 無制限)
  -trace           実行した命令の PC と命令語を逐一表示する
  -nb0-base addr   .nb0 イメージのロード先アドレス (default 0x30000000)
`)
	os.Exit(2)
}

func main() {
	if len(os.Args) < 2 {
		usage()
	}
	switch os.Args[1] {
	case "info":
		cmdInfo(os.Args[2:])
	case "run":
		cmdRun(os.Args[2:])
	default:
		usage()
	}
}

func fatal(err error) {
	fmt.Fprintln(os.Stderr, "cerulean:", err)
	os.Exit(1)
}

// デフォルトの .nb0 ロード先。SMDK2410 の SDRAM 先頭。
// TODO: 実イメージ入手後に正しいベース（イメージ配布物の設定）を確認する。
const defaultNB0Base = 0x30000000

func cmdInfo(args []string) {
	fs := flag.NewFlagSet("info", flag.ExitOnError)
	nb0Base := fs.Uint64("nb0-base", defaultNB0Base, ".nb0 のロード先アドレス")
	_ = fs.Parse(args)
	if fs.NArg() != 1 {
		usage()
	}
	path := fs.Arg(0)

	// BIN 形式ならレコード一覧も出したいので、Load ではなく形式判別を自前で行う。
	f, err := os.Open(path)
	if err != nil {
		fatal(err)
	}
	defer f.Close()

	var magic [7]byte
	if n, _ := f.Read(magic[:]); n == 7 && string(magic[:]) == "B000FF\x0A" {
		if _, err := f.Seek(0, 0); err != nil {
			fatal(err)
		}
		img, err := loader.LoadBIN(f)
		if err != nil {
			fatal(err)
		}
		fmt.Printf("format:  Windows CE BIN (B000FF)\n")
		fmt.Printf("start:   %08X\n", img.Start)
		fmt.Printf("length:  %08X (%d bytes)\n", img.Length, img.Length)
		fmt.Printf("entry:   %08X\n", img.Entry)
		fmt.Printf("records: %d\n", len(img.Records))
		for i, r := range img.Records {
			fmt.Printf("  [%3d] addr=%08X len=%8d checksum=%08X\n", i, r.Addr, r.Len, r.Checksum)
		}
		return
	}

	img, err := loader.Load(path, uint32(*nb0Base))
	if err != nil {
		fatal(err)
	}
	fmt.Printf("format:  raw (%s)\n", img.Format)
	fmt.Printf("start:   %08X\n", img.Start)
	fmt.Printf("length:  %08X (%d bytes)\n", img.Length, img.Length)
	fmt.Printf("entry:   %08X\n", img.Entry)
}

func cmdRun(args []string) {
	fs := flag.NewFlagSet("run", flag.ExitOnError)
	machineName := fs.String("machine", "smdk2410", "マシン構成")
	maxSteps := fs.Uint64("max-steps", 0, "最大実行命令数 (0 = 無制限)")
	trace := fs.Bool("trace", false, "実行トレースを表示")
	nb0Base := fs.Uint64("nb0-base", defaultNB0Base, ".nb0 のロード先アドレス")
	_ = fs.Parse(args)
	if fs.NArg() != 1 {
		usage()
	}

	if *machineName != "smdk2410" {
		fatal(fmt.Errorf("unknown machine %q (available: smdk2410)", *machineName))
	}
	m, err := smdk2410.New(os.Stdout)
	if err != nil {
		fatal(err)
	}

	img, err := loader.Load(fs.Arg(0), uint32(*nb0Base))
	if err != nil {
		fatal(err)
	}
	if err := m.LoadImage(img); err != nil {
		fatal(err)
	}
	m.Reset()
	fmt.Fprintf(os.Stderr, "cerulean: %s: loaded %s image, entry %08X (PA %08X)\n",
		m.Name(), img.Format, img.Entry, m.CPU().PC())

	c := m.CPU()
	var steps uint64
	for {
		if *trace {
			// 命令語はバス経由で覗く（フェッチ前なので副作用はない）。
			word, _ := m.Bus().Read32(c.PC())
			fmt.Fprintf(os.Stderr, "%12d  PC=%08X  %08X\n", steps, c.PC(), word)
		}
		if err := m.Step(); err != nil {
			steps++
			reportStop(c.PC(), steps, err)
			dumpRegs(c)
			os.Exit(1)
		}
		steps++
		if *maxSteps != 0 && steps >= *maxSteps {
			fmt.Fprintf(os.Stderr, "cerulean: stopped after %d steps at PC=%08X (max-steps)\n", steps, c.PC())
			dumpRegs(c)
			return
		}
	}
}

func reportStop(pc uint32, steps uint64, err error) {
	var ue *arm.UndefinedError
	var be *bus.BusError
	switch {
	case errors.As(err, &ue):
		fmt.Fprintf(os.Stderr, "cerulean: stopped after %d steps: %v\n", steps, ue)
	case errors.As(err, &be):
		fmt.Fprintf(os.Stderr, "cerulean: stopped after %d steps at PC=%08X: %v\n", steps, pc, be)
	default:
		fmt.Fprintf(os.Stderr, "cerulean: stopped after %d steps at PC=%08X: %v\n", steps, pc, err)
	}
}

func dumpRegs(c interface{ Reg(int) uint32 }) {
	for i := 0; i < 16; i++ {
		fmt.Fprintf(os.Stderr, "  r%-2d=%08X", i, c.Reg(i))
		if i%4 == 3 {
			fmt.Fprintln(os.Stderr)
		}
	}
}
