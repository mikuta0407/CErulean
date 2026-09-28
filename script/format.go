package script

import (
	"fmt"
	"io"
	"strings"
)

// Format はイベント列を、Parse で読み戻せるスクリプトとして w に書く
// （操作の記録の書き出し用。ユーザー確認済み 2026-09）。
//
// 時刻はすべて絶対命令数（@<n>i）で書く。s/ms や相対時刻は換算の丸めが
// 入り得るので使わない（記録した命令境界をそのまま再現するため）。
// tap/press には畳まず、down/up・key down/up のまま書く。
// header の各行は "# " を付けたコメントとして先頭に書く。
func Format(w io.Writer, header []string, events []Event) error {
	var b strings.Builder
	for _, h := range header {
		for _, l := range strings.Split(h, "\n") {
			fmt.Fprintf(&b, "# %s\n", l)
		}
	}
	var prev uint64
	for i, ev := range events {
		if ev.Step < prev {
			return fmt.Errorf("script: event %d (step %d) is before the previous one (step %d)", i, ev.Step, prev)
		}
		prev = ev.Step
		fmt.Fprintf(&b, "@%di %s", ev.Step, ev.Kind)
		switch ev.Kind {
		case TouchDown, TouchMove:
			if ev.X < 0 || ev.Y < 0 {
				return fmt.Errorf("script: event %d: negative coordinate (%d,%d)", i, ev.X, ev.Y)
			}
			fmt.Fprintf(&b, " %d %d", ev.X, ev.Y)
		case KeyDown, KeyUp:
			if err := checkWord(ev.Key); err != nil {
				return fmt.Errorf("script: event %d: key %w", i, err)
			}
			fmt.Fprintf(&b, " %s", ev.Key)
		case Shot, Snap:
			if err := checkWord(ev.Path); err != nil {
				return fmt.Errorf("script: event %d: path %w", i, err)
			}
			fmt.Fprintf(&b, " %s", ev.Path)
		case TouchUp, Quit:
		default:
			return fmt.Errorf("script: event %d: unknown kind %d", i, ev.Kind)
		}
		b.WriteByte('\n')
	}
	_, err := io.WriteString(w, b.String())
	return err
}

// checkWord は 1 語として書けるか（空白・"#" を含むと Parse で分割・
// コメント扱いされて読み戻せない）。
func checkWord(s string) error {
	if s == "" || strings.ContainsAny(s, " \t\r\n#") {
		return fmt.Errorf("%q cannot be written as a single word", s)
	}
	return nil
}
