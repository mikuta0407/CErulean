// Package snapshottest はスナップショット実装のテスト補助。
package snapshottest

import (
	"bytes"
	"reflect"
	"sort"
	"testing"

	"github.com/mikuta0407/cerulean/snapshot"
)

// CheckFields は構造体 v のフィールドが saved（SaveState が保存するもの）と
// wiring（配線・設定・キャッシュ等、保存しない理由があるもの）で過不足なく
// 分類されているかを検査する。フィールドを追加して SaveState の更新を
// 忘れると、このテストが落ちる（保存漏れによる非決定性の予防）。
func CheckFields(t *testing.T, v any, saved, wiring []string) {
	t.Helper()
	typ := reflect.TypeOf(v)
	if typ.Kind() == reflect.Pointer {
		typ = typ.Elem()
	}
	listed := map[string]bool{}
	for _, f := range append(append([]string{}, saved...), wiring...) {
		if listed[f] {
			t.Errorf("%s: field %q listed twice", typ.Name(), f)
		}
		listed[f] = true
	}
	actual := map[string]bool{}
	for i := 0; i < typ.NumField(); i++ {
		name := typ.Field(i).Name
		actual[name] = true
		if !listed[name] {
			t.Errorf("%s.%s is not covered by the snapshot: save it in SaveState/LoadState (and bump StateVersion) or list it as wiring",
				typ.Name(), name)
		}
	}
	var stale []string
	for f := range listed {
		if !actual[f] {
			stale = append(stale, f)
		}
	}
	sort.Strings(stale)
	for _, f := range stale {
		t.Errorf("%s: listed field %q does not exist", typ.Name(), f)
	}
}

// Save は s 単体の状態をチャンク 1 個のスナップショットにしてバイト列で返す。
func Save(t *testing.T, s snapshot.Stateful) []byte {
	t.Helper()
	var buf bytes.Buffer
	w, err := snapshot.NewWriter(&buf, "test", "")
	if err != nil {
		t.Fatal(err)
	}
	if err := w.Chunk("c", s); err != nil {
		t.Fatal(err)
	}
	if err := w.Close(); err != nil {
		t.Fatal(err)
	}
	return buf.Bytes()
}

// Load は Save で作ったバイト列から s に状態を戻す。
func Load(t *testing.T, data []byte, s snapshot.Stateful) {
	t.Helper()
	r, err := snapshot.NewReader(bytes.NewReader(data))
	if err != nil {
		t.Fatal(err)
	}
	if err := r.Chunk("c", s); err != nil {
		t.Fatal(err)
	}
	if err := r.Close(); err != nil {
		t.Fatal(err)
	}
}

// RoundTrip は src を保存して dst に復元し、dst を保存し直したバイト列が
// 一致することを確かめる（保存→復元で状態が失われないこと）。
func RoundTrip(t *testing.T, src, dst snapshot.Stateful) {
	t.Helper()
	a := Save(t, src)
	Load(t, a, dst)
	b := Save(t, dst)
	if !bytes.Equal(a, b) {
		t.Errorf("snapshot round trip changed the state (%d vs %d bytes)", len(a), len(b))
	}
}
