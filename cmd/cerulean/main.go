// cerulean は PC 開発用の CLI フロントエンド。
// コア（loader/cpu/bus/machine）は UI 非依存なので、ここにだけ
// 標準入出力・フラグ処理などを置く。
package main

import (
	"errors"
	"flag"
	"fmt"
	"image/png"
	"math"
	"os"
	"path/filepath"
	"runtime/pprof"
	"strconv"
	"strings"
	"time"

	"github.com/mikuta0407/cerulean/bus"
	"github.com/mikuta0407/cerulean/cpu/arm"
	"github.com/mikuta0407/cerulean/loader"
	"github.com/mikuta0407/cerulean/machine/smdk2410"
)

func usage() {
	fmt.Fprintf(os.Stderr, `Usage:
  cerulean info <image>                 イメージの情報を表示する
  cerulean run [flags] <image>          イメージを実行する
  cerulean run -snap-load f [flags] [image]
                                        スナップショットから再開する

run flags:
  -machine name    マシン構成 (default "smdk2410")
  -max-steps n     最大実行命令数 (0 = 無制限)
  -trace           実行した命令の PC・命令語・ディスアセンブルを逐一表示する
  -trace-from n    -trace の表示を n 命令目から始める
  -sample n        n 命令ごとに PC・CPSR を 1 行表示する（停滞箇所の調査用）
  -rtc time        RTC の初期時刻（例 2006-01-02T15:04:05。既定はホストの現在時刻。
                   固定すると実行が完全に再現可能になる）
  -fb-out f.png    停止時（max-steps・エラー）に LCD のフレームバッファを PNG に書く
  -fb-every n      -fb-out と併用。n 命令ごとに f-<命令数>.png として連番で書く
  -cpuprofile f    Go の CPU プロファイルを f に書く（エミュレータ自体の性能調査用）
  -stats           停止時に実行速度（命令/秒・実時間比・アイドルスキップの割合）を表示する
  -no-idle-skip    アイドルループ（割り込み待ちのスピン）のスキップを無効にする。
                   スキップしても実行結果は同一。-trace 指定時は自動で無効
  -history n       停止時に直前 n 命令を表示する (default 16, 0 = 無効)
  -watch lo[-hi]   物理アドレス範囲へのアクセス（RAM/MMIO）を PC 付きで表示する
                   （複数指定可。例: -watch 0x500F0000-0x500FFFFF）
  -nb0-base addr   .nb0 イメージのロード先アドレス (default 0x30000000)
  -script f        入力スクリプト（タップ・キー・画面保存を仮想時刻で並べたもの。
                   書式は script パッケージのコメント参照）
  -snap-save f@t   仮想時刻 t（例 95s, 3500000000i）に全状態を f に保存する
  -touch-raw       スクリプトのタッチ座標を ADC の生値（0〜1023）として渡す（調査用）
  -snap-load f     スナップショット f から再開する（命令数・仮想時刻は保存時点から
                   継続）。image を指定すると保存時のイメージと同一かを照合する
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
	traceFrom := fs.Uint64("trace-from", 0, "トレース開始命令数")
	rtcFlag := fs.String("rtc", "", "RTC 初期時刻 (YYYY-MM-DDTHH:MM:SS、既定は現在時刻)")
	fbOut := fs.String("fb-out", "", "停止時にフレームバッファを書く PNG パス")
	fbEvery := fs.Uint64("fb-every", 0, "n 命令ごとにフレームバッファを連番 PNG で書く (0 = 無効)")
	cpuprofile := fs.String("cpuprofile", "", "CPU プロファイル出力先")
	sample := fs.Uint64("sample", 0, "n 命令ごとに PC を表示 (0 = 無効)")
	var watches rangeList
	fs.Var(&watches, "watch", "監視する物理アドレス範囲 lo[-hi]（複数可）")
	history := fs.Int("history", 16, "停止時に表示する直前の命令数 (0 = 無効)")
	nb0Base := fs.Uint64("nb0-base", defaultNB0Base, ".nb0 のロード先アドレス")
	scriptPath := fs.String("script", "", "入力スクリプト")
	snapSave := fs.String("snap-save", "", "f@t: 仮想時刻 t にスナップショットを f に保存")
	snapLoad := fs.String("snap-load", "", "スナップショットから再開")
	noIdleSkip := fs.Bool("no-idle-skip", false, "アイドルループのスキップを無効にする")
	stats := fs.Bool("stats", false, "停止時に実行速度（命令/秒・実時間比）を表示する")
	fs.BoolVar(&touchRaw, "touch-raw", false, "スクリプトのタッチ座標を ADC 生値として渡す（調査用）")
	_ = fs.Parse(args)
	if fs.NArg() > 1 || (fs.NArg() == 0 && *snapLoad == "") {
		usage()
	}
	if *snapLoad != "" && *rtcFlag != "" {
		fatal(errors.New("-rtc cannot be used with -snap-load (the RTC state comes from the snapshot)"))
	}

	if *cpuprofile != "" {
		f, err := os.Create(*cpuprofile)
		if err != nil {
			fatal(err)
		}
		if err := pprof.StartCPUProfile(f); err != nil {
			fatal(err)
		}
		// os.Exit では defer が走らないので、停止経路ごとに stopProfile を呼ぶ。
		stopProfile = func() {
			pprof.StopCPUProfile()
			f.Close()
		}
	}

	if *machineName != "smdk2410" {
		fatal(fmt.Errorf("unknown machine %q (available: smdk2410)", *machineName))
	}
	m, err := smdk2410.New(os.Stdout)
	if err != nil {
		fatal(err)
	}
	// トレースで CPSR（Thumb 状態）を見るため具象型で持つ（cmd は arm に依存してよい）。
	c := m.CPU().(*arm.Core)
	// 監視はスナップショットの復元より前に登録する。復元時の TLB は、監視中は
	// RAM ページを直接持たない（bus を経由させる）ようにして復元されるため。
	for _, w := range watches {
		// PC は実行中の命令の次（ARM: +4 / Thumb: +2）を指している。
		m.Bus().AddWatch(w.lo, w.hi, func(region string, addr uint32, size int, v uint32, write bool) {
			kind := "R"
			if write {
				kind = "W"
			}
			fmt.Fprintf(os.Stderr, "%12d  watch %s%d %-10s PA=%08X v=%08X  (next PC=%08X)\n",
				m.Steps(), kind, size*8, region, addr, v, c.PC())
		})
	}

	// imageID は元イメージの SHA-256。スナップショットに記録し、再開時に
	// 別イメージと取り違えていないかの照合に使う。
	var imageID string
	if fs.NArg() == 1 {
		if imageID, err = fileSHA256(fs.Arg(0)); err != nil {
			fatal(err)
		}
	}
	if *snapLoad != "" {
		id, err := loadSnapshot(m, *snapLoad)
		if err != nil {
			fatal(err)
		}
		if imageID != "" && id != imageID {
			fatal(fmt.Errorf("-snap-load: snapshot was taken with a different image (sha256 %s, %s is %s)",
				id, fs.Arg(0), imageID))
		}
		imageID = id
		fmt.Fprintf(os.Stderr, "cerulean: %s: resumed from %s at step %d, PC %08X\n",
			m.Name(), *snapLoad, m.Steps(), m.CPU().PC())
	} else {
		img, err := loader.Load(fs.Arg(0), uint32(*nb0Base))
		if err != nil {
			fatal(err)
		}
		if err := m.LoadImage(img); err != nil {
			fatal(err)
		}
		rtcTime := time.Now()
		if *rtcFlag != "" {
			if rtcTime, err = time.ParseInLocation("2006-01-02T15:04:05", *rtcFlag, time.Local); err != nil {
				fatal(fmt.Errorf("-rtc: %w", err))
			}
		}
		m.SetRTC(rtcTime)
		m.Reset()
		fmt.Fprintf(os.Stderr, "cerulean: %s: loaded %s image, entry %08X (PA %08X)\n",
			m.Name(), img.Format, img.Entry, m.CPU().PC())
	}

	screenW, screenH = m.TouchScreenSize()
	events, err := buildEvents(*scriptPath, *snapSave, m.Steps())
	if err != nil {
		fatal(err)
	}

	if *fbEvery != 0 && *fbOut == "" {
		fatal(errors.New("-fb-every requires -fb-out"))
	}
	dumpFB := func(path string) {
		if path == "" {
			return
		}
		if err := writeFramebuffer(m, path); err != nil {
			fmt.Fprintln(os.Stderr, "cerulean:", err)
		}
	}
	// 直前 N 命令の履歴（停止原因の調査用）。記録は CPU コアが PC だけ持ち、
	// 命令語は表示時に読む（毎命令のディスアセンブルを避けるため）。
	c.SetHistory(*history)
	dumpHistory := func() {
		h := c.History()
		if *history <= 0 {
			return
		}
		fmt.Fprintf(os.Stderr, "last %d instructions:\n", len(h))
		for _, e := range h {
			word, next := peekAt(m, e.PC, e.Thumb)
			fmt.Fprintf(os.Stderr, "  PC=%08X  %s  %s\n", e.PC, fmtWord(word, e.Thumb), disasm(word, next, e.PC, e.Thumb))
		}
	}
	// トレース・監視はスキップした命令を表示できないので、アイドルスキップを
	// 切る（-watch 中は MMU が RAM を直接持たないので元々スキップされない）。
	if *noIdleSkip || *trace {
		m.SetIdleSkip(false)
	}

	// stop は停止時の共通処理（レジスタ・履歴・画面の出力）。
	started, startSteps := time.Now(), m.Steps()
	stop := func() {
		reportPA(m, c.PC())
		dumpRegs(c)
		dumpHistory() // どこでループしているかの調査用
		dumpFB(*fbOut)
		stopProfile()
		if *stats {
			el := time.Since(started).Seconds()
			n := m.Steps() - startSteps
			fmt.Fprintf(os.Stderr, "cerulean: %d steps in %.2fs (%.1fM steps/s, %.2fx real time, idle-skipped %.1f%%)\n",
				n, el, float64(n)/el/1e6, float64(n)/float64(smdk2410.InstructionsPerSecond)/el,
				100*float64(m.IdleSkipped())/float64(max(n, 1)))
		}
	}
	// スクリプトのイベントは「その命令数に達した時点（次の命令の実行前）」に
	// 適用する。実行は machine.RunUntil に任せ、次に止まるべき命令数
	// （イベント・-sample・-fb-every・-max-steps・トレース開始のうち最も近いもの）
	// までまとめて進める。
	nextEv := 0
	for {
		steps := m.Steps()
		for nextEv < len(events) && events[nextEv].Step <= steps {
			ev := events[nextEv]
			nextEv++
			quit, err := applyEvent(m, ev, imageID)
			if err != nil {
				fmt.Fprintf(os.Stderr, "cerulean: script line %d: %v\n", ev.Line, err)
				stop()
				os.Exit(1)
			}
			if quit {
				fmt.Fprintf(os.Stderr, "cerulean: stopped after %d steps at PC=%08X (script quit)\n", steps, c.PC())
				stop()
				return
			}
		}
		target := uint64(math.MaxUint64)
		if nextEv < len(events) {
			target = events[nextEv].Step
		}
		if *sample != 0 {
			target = min(target, (steps / *sample + 1)**sample)
		}
		if *fbEvery != 0 {
			target = min(target, (steps / *fbEvery + 1)**fbEvery)
		}
		if *maxSteps != 0 {
			target = min(target, *maxSteps)
		}
		if *trace {
			if steps >= *traceFrom {
				word, next, thumb := peekInstr(m, c)
				fmt.Fprintf(os.Stderr, "%12d  PC=%08X  %s  %s\n", steps, c.PC(), fmtWord(word, thumb), disasm(word, next, c.PC(), thumb))
				target = steps + 1
			} else {
				target = min(target, *traceFrom)
			}
		}
		// 少なくとも 1 命令は進める（-max-steps が再開時点以下の場合など）。
		target = max(target, steps+1)
		if err := m.RunUntil(target); err != nil {
			reportStop(c.PC(), m.Steps(), err)
			stop()
			os.Exit(1)
		}
		steps = m.Steps()
		if *sample != 0 && steps%*sample == 0 {
			fmt.Fprintf(os.Stderr, "%12d  sample PC=%08X CPSR=%08X\n", steps, c.PC(), uint32(c.CPSR()))
		}
		if *fbEvery != 0 && steps%*fbEvery == 0 {
			dumpFB(numberedPath(*fbOut, steps))
		}
		if *maxSteps != 0 && steps >= *maxSteps {
			fmt.Fprintf(os.Stderr, "cerulean: stopped after %d steps at PC=%08X (max-steps)\n", steps, c.PC())
			stop()
			return
		}
	}
}

// writeFramebuffer は LCD の現在の表示内容を PNG で書く。PNG 化・ファイル
// 出力は OS 依存なので cmd の責務（コアは画像を返すだけ）。
func writeFramebuffer(m *smdk2410.Machine, path string) error {
	img, cfg, err := m.Framebuffer()
	if err != nil {
		return err
	}
	f, err := os.Create(path)
	if err != nil {
		return err
	}
	if err := png.Encode(f, img); err != nil {
		f.Close()
		return err
	}
	if err := f.Close(); err != nil {
		return err
	}
	fmt.Fprintf(os.Stderr, "cerulean: wrote %s (%v)\n", path, cfg)
	return nil
}

// numberedPath は "shot.png" → "shot-000123456789.png"（命令数を 12 桁で埋め、
// ファイル名順 = 時系列にする）。
func numberedPath(path string, steps uint64) string {
	ext := filepath.Ext(path)
	return fmt.Sprintf("%s-%012d%s", strings.TrimSuffix(path, ext), steps, ext)
}

// stopProfile は -cpuprofile 指定時にプロファイルを閉じる。
var stopProfile = func() {}

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

// peekInstr は次に実行する命令を CPU と同じ経路（MMU 変換込み）で覗く。
// フェッチ前なので副作用はない。Thumb 状態ならハーフワードとして取り出し、
// BL の対を表示するため直後のハーフワードも返す。
func peekInstr(m *smdk2410.Machine, c *arm.Core) (word, next uint32, thumb bool) {
	thumb = c.CPSR().T()
	word, next = peekAt(m, c.PC(), thumb)
	return word, next, thumb
}

// peekAt は pc の命令語（Thumb ならハーフワードと直後のハーフワード）を読む。
func peekAt(m *smdk2410.Machine, pc uint32, thumb bool) (word, next uint32) {
	if !thumb {
		word, _ = m.Peek32(pc)
		return word, 0
	}
	w0, _ := m.Peek32(pc &^ 3)
	if pc&2 == 0 {
		return w0 & 0xFFFF, w0 >> 16
	}
	w1, _ := m.Peek32(pc&^3 + 4)
	return w0 >> 16, w1 & 0xFFFF
}

func disasm(word, next, pc uint32, thumb bool) string {
	if thumb {
		return arm.DisasmThumb(word, next, pc)
	}
	return arm.Disasm(word, pc)
}

// fmtWord は命令語の表示（Thumb は 4 桁）。
func fmtWord(word uint32, thumb bool) string {
	if thumb {
		return fmt.Sprintf("    %04X", word)
	}
	return fmt.Sprintf("%08X", word)
}

// reportPA は停止位置の VA→PA 変換結果を表示する（どの物理ページの
// コードかを知るための調査用）。
func reportPA(m interface {
	Translate(uint32) (uint32, error)
}, pc uint32) {
	if pa, err := m.Translate(pc); err == nil {
		fmt.Fprintf(os.Stderr, "  PC VA %08X -> PA %08X\n", pc, pa)
	} else {
		fmt.Fprintf(os.Stderr, "  PC VA %08X -> %v\n", pc, err)
	}
}

// rangeList は -watch の繰り返し指定（"lo" または "lo-hi"）。
type rangeList []struct{ lo, hi uint32 }

func (r *rangeList) String() string { return fmt.Sprint(*r) }

func (r *rangeList) Set(s string) error {
	lo, hi, found := strings.Cut(s, "-")
	l, err := strconv.ParseUint(lo, 0, 32)
	if err != nil {
		return err
	}
	h := l
	if found {
		if h, err = strconv.ParseUint(hi, 0, 32); err != nil {
			return err
		}
	}
	if h < l {
		return fmt.Errorf("range %q: hi < lo", s)
	}
	*r = append(*r, struct{ lo, hi uint32 }{uint32(l), uint32(h)})
	return nil
}
