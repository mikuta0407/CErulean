package main

// 一致確認用の出力（docs/rust-migration-plan.md §5.1・§5.3）。
// Go 版と Rust 版の「ゲストから見える状態」を、形式に依存しない値で比べる。
// バイト列の並びと JSON の項目は testdata/golden/README.md が正で、
// Rust 版も同じものを出す。どれも状態を読むだけで、実行結果は変えない。

import (
	"crypto/sha256"
	"encoding/binary"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"hash"
	"io"
	"os"

	"github.com/mikuta0407/cerulean/bus"
	"github.com/mikuta0407/cerulean/cpu/arm"
	"github.com/mikuta0407/cerulean/emu"
	"github.com/mikuta0407/cerulean/machine/smdk2410"
)

// cpuDumpVersion は CPU 状態のダンプの並びの版数（README の定義を変えたら上げる）。
const cpuDumpVersion = 1

// cpuDump は CPU 状態のダンプ（README の「CPU 状態のダンプ」）。すべて LE。
func cpuDump(m *smdk2410.Machine) []byte {
	a := m.CPU().(*arm.Core).ArchRegs()
	b := binary.LittleEndian.AppendUint32(nil, cpuDumpVersion)
	b = binary.LittleEndian.AppendUint64(b, m.Steps())
	for _, part := range [][]uint32{
		a.R[:], a.Usr[:], a.Fiq[:], a.Irq[:], a.Svc[:], a.Abt[:], a.Und[:],
		{a.CPSR}, a.SPSR[:],
	} {
		for _, v := range part {
			b = binary.LittleEndian.AppendUint32(b, v)
		}
	}
	// CP15。Read は読むだけで MMU の状態を変えない。
	for _, crn := range []uint8{1, 2, 3, 5, 6, 13} {
		v, _ := m.MMU().Read(0, crn, 0, 0)
		b = binary.LittleEndian.AppendUint32(b, v)
	}
	return b
}

func sha256Hex(b []byte) string {
	s := sha256.Sum256(b)
	return hex.EncodeToString(s[:])
}

// ramSHA256 は SDRAM 全体（PA 0x30000000 から 128MB）の SHA-256。
func ramSHA256(m *smdk2410.Machine) string {
	ram, off, ok := m.Bus().RAM(0x30000000)
	if !ok {
		return ""
	}
	return sha256Hex(ram[off:])
}

// screenSHA256 は LCD の変換後の RGBA 画素列（行の詰め物なし）の SHA-256。
// 表示が無効なら空。
func screenSHA256(m *smdk2410.Machine) (sum string, w, h int) {
	img, err := m.Frame()
	if err != nil {
		return "", 0, 0
	}
	w, h = img.Rect.Dx(), img.Rect.Dy()
	s := sha256.New()
	for y := 0; y < h; y++ {
		off := img.PixOffset(img.Rect.Min.X, img.Rect.Min.Y+y)
		s.Write(img.Pix[off : off+4*w])
	}
	return hex.EncodeToString(s.Sum(nil)), w, h
}

// uartTap は UART1 の送信バイト列を数えながら SHA-256 を取る（出力先へも流す）。
type uartTap struct {
	out io.Writer
	sum hash.Hash
	n   uint64
}

func newUARTTap(out io.Writer) *uartTap { return &uartTap{out: out, sum: sha256.New()} }

func (u *uartTap) Write(p []byte) (int, error) {
	u.sum.Write(p)
	u.n += uint64(len(p))
	return u.out.Write(p)
}

// digest は今までの送信バイト列の SHA-256（hash.Hash の Sum は状態を変えない）。
func (u *uartTap) digest() string { return hex.EncodeToString(u.sum.Sum(nil)) }

// stopInfo は停止の種類（README の stop.kind）。
type stopInfo struct {
	Kind  string  `json:"kind"`
	PC    *uint32 `json:"pc,omitempty"`    // undefined
	Word  *uint32 `json:"word,omitempty"`  // undefined
	Addr  *uint32 `json:"addr,omitempty"`  // bus-error
	Write *bool   `json:"write,omitempty"` // bus-error
}

func stopFromError(err error) *stopInfo {
	var ue *arm.UndefinedError
	var be *bus.BusError
	var ee *emu.EventError
	switch {
	case errors.As(err, &ee):
		return &stopInfo{Kind: "event-error"}
	case errors.As(err, &ue):
		return &stopInfo{Kind: "undefined", PC: &ue.PC, Word: &ue.Word}
	case errors.As(err, &be):
		return &stopInfo{Kind: "bus-error", Addr: &be.Addr, Write: &be.Write}
	}
	return &stopInfo{Kind: "error"}
}

// resultRecord は -result に書く 1 行（JSON Lines）。
type resultRecord struct {
	Format       int       `json:"format"`
	Event        string    `json:"event"` // "checkpoint" | "stop"
	Steps        uint64    `json:"steps"`
	Stop         *stopInfo `json:"stop,omitempty"`
	CPU          string    `json:"cpu"` // CPU 状態のダンプ（16 進）
	CPUSHA256    string    `json:"cpu_sha256"`
	RAMSHA256    string    `json:"ram_sha256"`
	UART1SHA256  string    `json:"uart1_sha256"`
	UART1Bytes   uint64    `json:"uart1_bytes"`
	ScreenSHA256 string    `json:"screen_sha256"` // 表示無効なら ""
	ScreenW      int       `json:"screen_w"`
	ScreenH      int       `json:"screen_h"`
}

// resultFormat は JSON の項目の版数（README の定義を変えたら上げる）。
const resultFormat = 1

// resultWriter は -result の出力先。
type resultWriter struct {
	f    *os.File
	uart *uartTap
}

func (r *resultWriter) write(m *smdk2410.Machine, event string, stop *stopInfo) error {
	if r == nil {
		return nil
	}
	dump := cpuDump(m)
	scr, w, h := screenSHA256(m)
	rec := resultRecord{
		Format: resultFormat, Event: event, Steps: m.Steps(), Stop: stop,
		CPU: hex.EncodeToString(dump), CPUSHA256: sha256Hex(dump),
		RAMSHA256:   ramSHA256(m),
		UART1SHA256: r.uart.digest(), UART1Bytes: r.uart.n,
		ScreenSHA256: scr, ScreenW: w, ScreenH: h,
	}
	b, err := json.Marshal(rec)
	if err != nil {
		return err
	}
	_, err = r.f.Write(append(b, '\n'))
	return err
}

// traceHasher は -trace-hash の出力。every 命令ごとに「命令数 CPU ダンプの
// SHA-256」、ramEvery 命令ごとに RAM の SHA-256 も加えた 1 行を書く。
type traceHasher struct {
	w               io.Writer
	every, ramEvery uint64
}

// due は steps が出力する命令数か。
func (t *traceHasher) due(steps uint64) (cpu, ram bool) {
	if t == nil || steps == 0 {
		return false, false
	}
	ram = t.ramEvery != 0 && steps%t.ramEvery == 0
	cpu = ram || (t.every != 0 && steps%t.every == 0)
	return cpu, ram
}

// next は steps より後で次に出力する命令数（実行ループの止まる点）。
func (t *traceHasher) next(steps uint64) uint64 {
	n := ^uint64(0)
	if t == nil {
		return n
	}
	if t.every != 0 {
		n = min(n, nextMultiple(steps, t.every))
	}
	if t.ramEvery != 0 {
		n = min(n, nextMultiple(steps, t.ramEvery))
	}
	return n
}

func (t *traceHasher) emit(m *smdk2410.Machine) error {
	cpu, ram := t.due(m.Steps())
	if !cpu {
		return nil
	}
	line := fmt.Sprintf("%d %s", m.Steps(), sha256Hex(cpuDump(m)))
	if ram {
		line += " ram=" + ramSHA256(m)
	}
	_, err := fmt.Fprintln(t.w, line)
	return err
}
