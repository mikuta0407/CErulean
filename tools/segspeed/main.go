// segspeed は入力スクリプトを再生し、仮想時間の一定区間（既定 0.25 秒）ごとの
// 実時間比・アイドル割合・実処理の命令/秒を表示する（性能の調査用。操作の
// どの区間が遅いかを見る）。スクリプトの shot/snap は無視し、quit で止まる
// （quit が無いと止まらない）。
//
//	go run ./tools/segspeed [-seg 0.25] [-cpuprofile f] <snapshot> <script>
//
// 計画書 §6.3 の「taps-5 の操作区間の再生時間と、タップ直後の区間の実時間比」を
// 測るのに使う。Rust 版には CLI のサブコマンドとして同じものを用意する。
package main

import (
	"flag"
	"fmt"
	"os"
	"runtime/pprof"
	"time"

	"github.com/mikuta0407/cerulean/emu"
	"github.com/mikuta0407/cerulean/machine"
	"github.com/mikuta0407/cerulean/machine/smdk2410"
	"github.com/mikuta0407/cerulean/script"
)

func main() {
	seg := flag.Float64("seg", 0.25, "区間の長さ（仮想秒）")
	cpuprofile := flag.String("cpuprofile", "", "Go の CPU プロファイルの出力先")
	flag.Parse()
	if flag.NArg() != 2 {
		fmt.Fprintln(os.Stderr, "usage: segspeed [-seg sec] [-cpuprofile f] <snapshot> <script>")
		os.Exit(2)
	}
	if err := run(flag.Arg(0), flag.Arg(1), *seg, *cpuprofile); err != nil {
		fmt.Fprintln(os.Stderr, "segspeed:", err)
		os.Exit(1)
	}
}

func run(snapPath, scriptPath string, segSec float64, cpuprofile string) error {
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
	sf, err := os.Open(scriptPath)
	if err != nil {
		return err
	}
	evs, err := script.Parse(sf, smdk2410.InstructionsPerSecond)
	sf.Close()
	if err != nil {
		return err
	}
	if cpuprofile != "" {
		pf, err := os.Create(cpuprofile)
		if err != nil {
			return err
		}
		defer pf.Close()
		if err := pprof.StartCPUProfile(pf); err != nil {
			return err
		}
		defer pprof.StopCPUProfile()
	}
	s := emu.New(m)
	s.Apply = func(m machine.Machine, ev script.Event) (bool, error) {
		switch ev.Kind {
		case script.Shot, script.Snap:
			return false, nil
		case script.Quit:
			return true, nil
		}
		fmt.Printf("   >>> %v %d %d %s\n", ev.Kind, ev.X, ev.Y, ev.Key)
		return emu.ApplyInput(m, ev)
	}
	// 再開点より前のイベントは適用済みとみなす（cerulean run と同じ）。
	for len(evs) > 0 && evs[0].Step < m.Steps() {
		evs = evs[1:]
	}
	s.Schedule(evs...)
	ips := float64(smdk2410.InstructionsPerSecond)
	segSteps := uint64(segSec * ips)
	if segSteps == 0 {
		return fmt.Errorf("-seg too small")
	}
	t0 := m.Steps()
	total := time.Now()
	for {
		st, sk, w := m.Steps(), m.IdleSkipped(), time.Now()
		quit, err := s.Run(st + segSteps)
		if err != nil {
			return err
		}
		n := m.Steps() - st
		el := time.Since(w).Seconds()
		busy := n - (m.IdleSkipped() - sk)
		fmt.Printf("v=%6.2fs  ratio=%5.2fx  busy=%5.1f%%  busyMIPS=%5.1f\n",
			float64(m.Steps()-t0)/ips, float64(n)/ips/el,
			100*float64(busy)/float64(max(n, 1)), float64(busy)/el/1e6)
		if quit {
			break
		}
	}
	fmt.Printf("total %.1fs wall for %.1fs virtual\n", time.Since(total).Seconds(), float64(m.Steps()-t0)/ips)
	return nil
}
