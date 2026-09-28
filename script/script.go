// Package script は決定論的な入力スクリプト（タップ・キー操作・画面保存を
// 仮想時刻つきで並べたテキスト）の解釈。OS に依存しない（io.Reader から
// 読むだけ）ので、CLI 以外（gomobile 側のリプレイ等）からも使える。
//
// 書式（ユーザー確認済み 2026-09）: 1 行 1 コマンド、"#" 以降はコメント。
//
//	<時刻> <コマンド> [引数...]
//
// 時刻:
//
//	@<量>  絶対時刻（リセットからの仮想時間）
//	+<量>  直前のコマンドの終了時刻からの相対（tap/press は押下時間の後が終了）
//	量の単位: s（秒）・ms（ミリ秒）・i（命令数）。s/ms は小数可（例 1.5s）。
//
// 仮想時間は命令数から固定比で決まる（machine の InstructionsPerSecond）ので、
// 時刻はすべて命令数に換算して扱う。
//
// コマンド:
//
//	tap <x> <y> [押下時間]   down → 押下時間後に up（既定 100ms）
//	down <x> <y> / move <x> <y> / up   ペンの押下・移動・解放
//	key down <名前> / key up <名前>     キーの押下・解放
//	press <名前> [押下時間]  key down → 押下時間後に key up（既定 100ms）
//	shot <ファイル>          フレームバッファを PNG で保存
//	snap <ファイル>          スナップショットを保存
//	quit                     実行を終了
//
// 座標は LCD のピクセル座標（左上原点）。範囲やキー名の妥当性はマシン
// 依存なので、ここでは検査しない（呼び出し側が実行前に検査する）。
package script

import (
	"bufio"
	"fmt"
	"io"
	"math/bits"
	"strconv"
	"strings"
)

// Kind はイベントの種類。
type Kind int

const (
	TouchDown Kind = iota
	TouchMove
	TouchUp
	KeyDown
	KeyUp
	Shot
	Snap
	Quit
)

func (k Kind) String() string {
	return [...]string{"down", "move", "up", "key down", "key up", "shot", "snap", "quit"}[k]
}

// Event は 1 個の入力イベント。tap/press は 2 個のイベントに展開される。
type Event struct {
	Step uint64 // 実行する時刻（リセットからの命令数）。この命令数に達した時点で適用する
	Kind Kind
	X, Y int    // TouchDown/TouchMove
	Key  string // KeyDown/KeyUp
	Path string // Shot/Snap
	Line int    // 元の行番号（エラー表示用）
}

// DefaultHold は tap/press の既定の押下時間（ミリ秒）。
// 100ms は人のタップとして自然な長さで、ドライバのサンプリング間隔
// （TODO: touch.dll の実測値で見直す）より十分長い値として選んだ。
const DefaultHold = 100

// Parse は r のスクリプトを解釈し、時刻順（=記述順）のイベント列を返す。
// stepsPerSecond は仮想時間 1 秒あたりの命令数。
func Parse(r io.Reader, stepsPerSecond uint64) ([]Event, error) {
	var (
		events []Event
		now    uint64 // 直前のコマンドの終了時刻
		lineNo int
	)
	sc := bufio.NewScanner(r)
	for sc.Scan() {
		lineNo++
		line := sc.Text()
		if i := strings.IndexByte(line, '#'); i >= 0 {
			line = line[:i]
		}
		f := strings.Fields(line)
		if len(f) == 0 {
			continue
		}
		errf := func(format string, args ...any) error {
			return fmt.Errorf("script line %d: %s", lineNo, fmt.Sprintf(format, args...))
		}
		if len(f) < 2 {
			return nil, errf("expected <time> <command>")
		}
		at, err := parseTime(f[0], now, stepsPerSecond)
		if err != nil {
			return nil, errf("%v", err)
		}
		if at < now {
			return nil, errf("time %s is before the previous command (step %d < %d)", f[0], at, now)
		}
		now = at
		ev := func(k Kind) Event { return Event{Step: at, Kind: k, Line: lineNo} }
		args := f[2:]
		switch cmd := f[1]; cmd {
		case "tap", "down", "move":
			min, max := 2, 2
			if cmd == "tap" {
				max = 3
			}
			if len(args) < min || len(args) > max {
				return nil, errf("usage: %s <x> <y>%s", cmd, map[bool]string{true: " [hold]"}[cmd == "tap"])
			}
			x, y, err := parseXY(args[0], args[1])
			if err != nil {
				return nil, errf("%v", err)
			}
			kind := map[string]Kind{"tap": TouchDown, "down": TouchDown, "move": TouchMove}[cmd]
			e := ev(kind)
			e.X, e.Y = x, y
			events = append(events, e)
			if cmd == "tap" {
				hold, err := holdSteps(args[2:], stepsPerSecond)
				if err != nil {
					return nil, errf("%v", err)
				}
				now += hold
				up := ev(TouchUp)
				up.Step = now
				events = append(events, up)
			}
		case "up":
			if len(args) != 0 {
				return nil, errf("usage: up")
			}
			events = append(events, ev(TouchUp))
		case "key":
			if len(args) != 2 || (args[0] != "down" && args[0] != "up") {
				return nil, errf("usage: key down|up <name>")
			}
			e := ev(KeyDown)
			if args[0] == "up" {
				e.Kind = KeyUp
			}
			e.Key = args[1]
			events = append(events, e)
		case "press":
			if len(args) < 1 || len(args) > 2 {
				return nil, errf("usage: press <name> [hold]")
			}
			hold, err := holdSteps(args[1:], stepsPerSecond)
			if err != nil {
				return nil, errf("%v", err)
			}
			down := ev(KeyDown)
			down.Key = args[0]
			now += hold
			up := ev(KeyUp)
			up.Key = args[0]
			up.Step = now
			events = append(events, down, up)
		case "shot", "snap":
			if len(args) != 1 {
				return nil, errf("usage: %s <file>", cmd)
			}
			e := ev(Shot)
			if cmd == "snap" {
				e.Kind = Snap
			}
			e.Path = args[0]
			events = append(events, e)
		case "quit":
			if len(args) != 0 {
				return nil, errf("usage: quit")
			}
			events = append(events, ev(Quit))
		default:
			return nil, errf("unknown command %q", cmd)
		}
	}
	if err := sc.Err(); err != nil {
		return nil, err
	}
	return events, nil
}

func parseXY(xs, ys string) (int, int, error) {
	x, err := strconv.Atoi(xs)
	if err != nil || x < 0 {
		return 0, 0, fmt.Errorf("bad x coordinate %q", xs)
	}
	y, err := strconv.Atoi(ys)
	if err != nil || y < 0 {
		return 0, 0, fmt.Errorf("bad y coordinate %q", ys)
	}
	return x, y, nil
}

func holdSteps(args []string, stepsPerSecond uint64) (uint64, error) {
	if len(args) == 0 {
		return DefaultHold * stepsPerSecond / 1000, nil
	}
	d, err := ParseDuration(args[0], stepsPerSecond)
	if err != nil {
		return 0, err
	}
	if d == 0 {
		return 0, fmt.Errorf("hold time must be > 0")
	}
	return d, nil
}

func parseTime(s string, now, stepsPerSecond uint64) (uint64, error) {
	if len(s) < 2 || (s[0] != '@' && s[0] != '+') {
		return 0, fmt.Errorf("time %q must start with @ (absolute) or + (relative)", s)
	}
	d, err := ParseDuration(s[1:], stepsPerSecond)
	if err != nil {
		return 0, err
	}
	if s[0] == '+' {
		return now + d, nil
	}
	return d, nil
}

// ParseDuration は "95s"・"1.5s"・"250ms"・"3500000000i" を命令数に換算する。
// 小数は 10 進のまま整数演算で換算し（浮動小数の丸めで命令数が環境依存に
// ならないように）、端数の命令は切り捨てる。
func ParseDuration(s string, stepsPerSecond uint64) (uint64, error) {
	var num string
	var unitNum, unitDen uint64 // 1 単位 = unitNum/unitDen 命令
	switch {
	case strings.HasSuffix(s, "ms"):
		num, unitNum, unitDen = strings.TrimSuffix(s, "ms"), stepsPerSecond, 1000
	case strings.HasSuffix(s, "s"):
		num, unitNum, unitDen = strings.TrimSuffix(s, "s"), stepsPerSecond, 1
	case strings.HasSuffix(s, "i"):
		num, unitNum, unitDen = strings.TrimSuffix(s, "i"), 1, 1
	default:
		return 0, fmt.Errorf("duration %q needs a unit (s, ms or i)", s)
	}
	intPart, fracPart, hasFrac := strings.Cut(num, ".")
	if intPart == "" || (hasFrac && fracPart == "") {
		return 0, fmt.Errorf("bad number in duration %q", s)
	}
	if hasFrac && unitDen == 1 && unitNum == 1 {
		return 0, fmt.Errorf("instruction count %q must be an integer", s)
	}
	digits := intPart + fracPart
	for _, c := range digits {
		if c < '0' || c > '9' {
			return 0, fmt.Errorf("bad number in duration %q", s)
		}
	}
	if len(fracPart) > 9 {
		return 0, fmt.Errorf("too many decimal places in %q", s)
	}
	v, err := strconv.ParseUint(digits, 10, 64)
	if err != nil {
		return 0, fmt.Errorf("bad number in duration %q", s)
	}
	den := unitDen
	for range fracPart {
		den *= 10
	}
	// v × unitNum / den。オーバーフローを避けるため商と余りに分ける。
	q, r := v/den, v%den
	hi := q * unitNum
	if unitNum != 0 && hi/unitNum != q {
		return 0, fmt.Errorf("duration %q too large", s)
	}
	// r×unitNum は 64 ビットを超え得るので 128 ビットで計算する
	// （r < den なので商は unitNum 未満に収まる）。
	ph, pl := bits.Mul64(r, unitNum)
	frac, _ := bits.Div64(ph, pl, den)
	return hi + frac, nil
}
