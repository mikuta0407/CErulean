package main

import (
	"crypto/sha256"
	"encoding/hex"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"strings"

	"github.com/mikuta0407/cerulean/emu"
	"github.com/mikuta0407/cerulean/machine/smdk2410"
	"github.com/mikuta0407/cerulean/script"
)

// fileSHA256 はイメージファイルの SHA-256（16 進）。スナップショットの
// imageID に使う。
func fileSHA256(path string) (string, error) {
	f, err := os.Open(path)
	if err != nil {
		return "", err
	}
	defer f.Close()
	h := sha256.New()
	if _, err := io.Copy(h, f); err != nil {
		return "", err
	}
	return hex.EncodeToString(h.Sum(nil)), nil
}

func loadSnapshot(m *smdk2410.Machine, path string) (string, error) {
	f, err := os.Open(path)
	if err != nil {
		return "", err
	}
	defer f.Close()
	id, err := m.LoadSnapshot(f)
	if err != nil {
		return "", fmt.Errorf("-snap-load %s: %w", path, err)
	}
	return id, nil
}

// saveSnapshot は一時ファイルに書いてから rename する（途中で失敗しても
// 壊れたスナップショットが残らないように）。
func saveSnapshot(m *smdk2410.Machine, path, imageID string) error {
	tmp, err := os.CreateTemp(filepath.Dir(path), filepath.Base(path)+".tmp*")
	if err != nil {
		return err
	}
	defer os.Remove(tmp.Name()) // rename 成功後は存在しないので無害
	if err := m.SaveSnapshot(tmp, imageID); err != nil {
		tmp.Close()
		return err
	}
	if err := tmp.Close(); err != nil {
		return err
	}
	if err := os.Chmod(tmp.Name(), 0o644); err != nil { // CreateTemp は 0600 で作る
		return err
	}
	if err := os.Rename(tmp.Name(), path); err != nil {
		return err
	}
	fmt.Fprintf(os.Stderr, "cerulean: saved snapshot %s at step %d\n", path, m.Steps())
	return nil
}

// buildEvents は -script と -snap-save からイベント列を作る。start
// （スナップショットから再開した時点の命令数）より前のイベントは、保存
// 前の実行で適用済みとみなして読み飛ばす（同じスクリプトを再開に使い
// 回せるように。件数は表示する）。
func buildEvents(m *smdk2410.Machine, scriptPath, snapSave string, start uint64) ([]script.Event, error) {
	var events []script.Event
	if scriptPath != "" {
		f, err := os.Open(scriptPath)
		if err != nil {
			return nil, err
		}
		events, err = script.Parse(f, smdk2410.InstructionsPerSecond)
		f.Close()
		if err != nil {
			return nil, fmt.Errorf("%s: %w", scriptPath, err)
		}
	}
	if snapSave != "" {
		path, at, ok := strings.Cut(snapSave, "@")
		if !ok || path == "" {
			return nil, fmt.Errorf("-snap-save: want file@time (e.g. boot.snap@95s), got %q", snapSave)
		}
		step, err := script.ParseDuration(at, smdk2410.InstructionsPerSecond)
		if err != nil {
			return nil, fmt.Errorf("-snap-save: %w", err)
		}
		events = insertEvent(events, script.Event{Step: step, Kind: script.Snap, Path: path})
	}
	for _, ev := range events {
		if err := validateEvent(m, ev); err != nil {
			return nil, fmt.Errorf("script line %d: %w", ev.Line, err)
		}
	}
	skip := 0
	for skip < len(events) && events[skip].Step < start {
		skip++
	}
	if skip > 0 {
		fmt.Fprintf(os.Stderr, "cerulean: skipped %d script events before the resume point (step %d)\n", skip, start)
	}
	return events[skip:], nil
}

// insertEvent は時刻順を保って ev を挿入する（同時刻なら既存の後）。
func insertEvent(events []script.Event, ev script.Event) []script.Event {
	i := len(events)
	for i > 0 && events[i-1].Step > ev.Step {
		i--
	}
	events = append(events, script.Event{})
	copy(events[i+1:], events[i:])
	events[i] = ev
	return events
}

// validateEvent は実行前にマシン依存の妥当性（座標範囲・キー名）を検査する
// （-touch-raw のときだけ座標を ADC 生値の範囲で見る）。
func validateEvent(m *smdk2410.Machine, ev script.Event) error {
	if touchRaw && (ev.Kind == script.TouchDown || ev.Kind == script.TouchMove) {
		if ev.X > 1023 || ev.Y > 1023 {
			return fmt.Errorf("%v: raw ADC value out of range (0-1023): %d %d", ev.Kind, ev.X, ev.Y)
		}
		return nil
	}
	return emu.Validate(m, ev)
}

// touchRaw は -touch-raw（スクリプトの座標を ADC 生値として渡す調査用モード）。
var touchRaw bool

// lastRaw は move/up で位置を引き継ぐための直前のペン位置。
var lastRawX, lastRawY uint32

// applyEvent はイベントを 1 個適用する。quit なら true。
func applyEvent(m *smdk2410.Machine, ev script.Event, imageID string) (quit bool, err error) {
	switch ev.Kind {
	case script.Shot:
		return false, writeFramebuffer(m, ev.Path)
	case script.Snap:
		return false, saveSnapshot(m, ev.Path, imageID)
	case script.Quit:
		return true, nil
	case script.TouchDown, script.TouchMove:
		if touchRaw {
			lastRawX, lastRawY = uint32(ev.X), uint32(ev.Y)
			m.TouchRaw(true, lastRawX, lastRawY)
			return false, nil
		}
	case script.TouchUp:
		if touchRaw {
			m.TouchRaw(false, lastRawX, lastRawY)
			return false, nil
		}
	}
	return emu.ApplyInput(m, ev)
}
