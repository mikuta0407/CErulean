// Package emu はフロントエンド共通の実行制御: 命令数つきの入力イベントを
// 命令境界で適用しながらマシンを進め、対話入力を記録する。
//
// CLI（スクリプト再生）・ブラウザ型フロントエンド・将来の gomobile から
// 同じ規則で使うためのもので、純 Go・OS 非依存（ファイル IO・壁時計は
// 呼び出し側の責務）。決定論性の要は次の 1 点だけ:
//
//	イベントは「Steps() がその命令数に達した時点（次の命令の実行前）」に適用する。
//
// 対話入力（Inject）は適用した時点の Steps() を刻んで記録するので、記録を
// スクリプトとして再生すると、同じ命令境界で同じ入力が入り、同じ状態になる。
package emu

import (
	"fmt"
	"io"
	"slices"
	"strings"

	"github.com/mikuta0407/cerulean/machine"
	"github.com/mikuta0407/cerulean/script"
)

// Session は 1 台のマシンと、予定されたイベント列・入力の記録を持つ。
// goroutine 安全ではない（1 つの goroutine が所有して使うこと）。
type Session struct {
	m machine.Machine

	events []script.Event // 予定（Step 昇順）。先頭 next 件は適用済み
	next   int

	// Apply はイベントを 1 個適用する。既定は ApplyInput（入力のみ扱い、
	// shot/snap/quit はエラー）。ファイル出力などを扱う呼び出し側が
	// 差し替え、入力は ApplyInput に委ねる。quit=true で Run が止まる。
	Apply func(m machine.Machine, ev script.Event) (quit bool, err error)

	recording bool
	record    []script.Event
	recStart  uint64
}

// New は m を操作するセッションを作る。
func New(m machine.Machine) *Session {
	return &Session{m: m, Apply: ApplyInput}
}

// Machine は操作対象のマシン。
func (s *Session) Machine() machine.Machine { return s.m }

// EventError は予定したイベントの適用に失敗したことを表す。
type EventError struct {
	Event script.Event
	Err   error
}

func (e *EventError) Error() string {
	return fmt.Sprintf("script line %d: %v", e.Event.Line, e.Err)
}

func (e *EventError) Unwrap() error { return e.Err }

// Schedule はイベントを予定に加える（Step 順。同じ命令数なら加えた順）。
// 現在の Steps() より前のイベントは、次の Run の最初に適用される。
func (s *Session) Schedule(evs ...script.Event) {
	for _, ev := range evs {
		i := len(s.events)
		for i > s.next && s.events[i-1].Step > ev.Step {
			i--
		}
		s.events = slices.Insert(s.events, i, ev)
	}
}

// Pending は未適用の予定イベント数。
func (s *Session) Pending() int { return len(s.events) - s.next }

// NextEventStep は次の予定イベントの命令数（なければ ok=false）。
func (s *Session) NextEventStep() (step uint64, ok bool) {
	if s.next < len(s.events) {
		return s.events[s.next].Step, true
	}
	return 0, false
}

// Run は予定イベントを命令境界で適用しながら、Steps() が until に達するまで
// 進める。until ちょうどに予定されたイベントは適用せずに戻る（次の Run の
// 最初に適用される）。quit イベントを適用したら quit=true で直ちに戻る。
func (s *Session) Run(until uint64) (quit bool, err error) {
	for {
		steps := s.m.Steps()
		for s.next < len(s.events) && s.events[s.next].Step <= steps {
			ev := s.events[s.next]
			s.next++
			q, err := s.Apply(s.m, ev)
			if err != nil {
				return false, &EventError{Event: ev, Err: err}
			}
			if q {
				return true, nil
			}
		}
		if steps >= until {
			return false, nil
		}
		target := until
		if next, ok := s.NextEventStep(); ok && next < target {
			target = next
		}
		if err := s.m.RunUntil(target); err != nil {
			return false, err
		}
		if s.m.Steps() >= until {
			return false, nil // until ちょうどのイベントは次の Run で適用する
		}
	}
}

// Inject は対話入力を今の命令境界（Steps()）で直ちに適用し、記録中なら
// 記録する。ev.Step は上書きされる。入力（down/move/up/key）以外は不可。
func (s *Session) Inject(ev script.Event) error {
	if !isInput(ev.Kind) {
		return fmt.Errorf("emu: %v cannot be injected", ev.Kind)
	}
	if err := Validate(s.m, ev); err != nil {
		return err
	}
	ev.Step = s.m.Steps()
	ev.Line = 0
	if _, err := ApplyInput(s.m, ev); err != nil {
		return err
	}
	if s.recording {
		s.record = append(s.record, ev)
	}
	return nil
}

// StartRecording は記録を始める（それまでの記録は捨てる）。再生の起点に
// なる状態（スナップショット）は呼び出し側が同じ命令境界で保存すること。
func (s *Session) StartRecording() {
	s.recording = true
	s.record = nil
	s.recStart = s.m.Steps()
}

// Recording は記録中か。
func (s *Session) Recording() bool { return s.recording }

// StopRecording は記録を止め、記録開始の命令数と記録したイベントを返す。
func (s *Session) StopRecording() (start uint64, events []script.Event) {
	s.recording = false
	ev := s.record
	s.record = nil
	return s.recStart, ev
}

func isInput(k script.Kind) bool {
	switch k {
	case script.TouchDown, script.TouchMove, script.TouchUp, script.KeyDown, script.KeyUp:
		return true
	}
	return false
}

// ApplyInput は入力イベントをマシンに適用する（Session.Apply の既定）。
// shot/snap/quit はファイル出力や終了を伴うので呼び出し側が扱う。
func ApplyInput(m machine.Machine, ev script.Event) (quit bool, err error) {
	switch ev.Kind {
	case script.TouchDown, script.TouchMove:
		// ペンが上がっていれば move も down と同じ（machine の規約）。
		return false, m.TouchMove(ev.X, ev.Y)
	case script.TouchUp:
		m.TouchUp()
		return false, nil
	case script.KeyDown:
		return false, m.KeyDown(ev.Key)
	case script.KeyUp:
		return false, m.KeyUp(ev.Key)
	}
	return false, fmt.Errorf("%v: not handled", ev.Kind)
}

// Validate は実行前にマシン依存の妥当性（座標範囲・キー名）を検査する
// （長い実行の途中でスクリプトの誤りに気づくのを避けるため）。
func Validate(m machine.Machine, ev script.Event) error {
	switch ev.Kind {
	case script.TouchDown, script.TouchMove:
		w, h := m.TouchScreenSize()
		if ev.X < 0 || ev.Y < 0 || ev.X >= w || ev.Y >= h {
			return fmt.Errorf("%v: (%d,%d) outside the %dx%d screen", ev.Kind, ev.X, ev.Y, w, h)
		}
	case script.KeyDown, script.KeyUp:
		names := m.KeyNames()
		if !slices.Contains(names, ev.Key) {
			return fmt.Errorf("unknown key %q (available: %s)", ev.Key, strings.Join(names, " "))
		}
	}
	return nil
}

// Format は記録（StopRecording の結果）を、再生用のスクリプトとして w に書く。
// 先頭のコメントに再生の起点（記録開始時点のスナップショット）を残す。
// 時刻は絶対命令数なので、同じスナップショットから再生すれば同じ命令境界で
// 同じ入力が入る。
func Format(w io.Writer, startSnap, imageID string, start uint64, events []script.Event) error {
	header := []string{
		"CErulean input recording",
		fmt.Sprintf("start step: %d", start),
		"start snapshot: " + startSnap,
	}
	if imageID != "" {
		header = append(header, "image sha256: "+imageID)
	}
	header = append(header, "replay: cerulean run -snap-load <start snapshot> -script <this file>")
	return script.Format(w, header, events)
}
