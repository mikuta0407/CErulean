package smdk2410

import (
	"fmt"
	"sort"

	"github.com/mikuta0407/cerulean/snapshot"
)

// kbdMCU は SPI1 につながるキーボード用マイコン（SMDK2410 ボード上の部品で、
// S3C2410 のデータシートの範囲外）。kbdmouse.dll が SPI1 と EINT1 で使う。
//
// 観察した事実（2026-09、実イメージの kbdmouse.dll をトレース）:
//   - 初期化: GPG5〜7 を SPI1、GPB6 を出力（チップセレクト）、GPF1 を EINT1
//     （立ち下がりエッジ）にし、SPCON1=0x3A・SPPRE1=0xFF。0xFF を 10 回送って
//     受信を読み捨て、その後 3 バイトのコマンド（1B A0 7B、1B A1 7A 等。
//     3 バイトの XOR が 0xC0）を送る。コマンドの意味は未解明（応答も読まない）。
//   - EINT1 の IST は 1 回の割り込みで 1 バイトだけ読む: GPB6=Low → SPTDAT1 に
//     0xFF → REDY 待ち → GPB6=High → SPRDAT1 を読む。
//   - バイトの bit7=1 はキーを離した、bit6〜0 はスキャンコード。直前と同じ
//     バイトは無視する（二重通知の除去）。
//   - スキャンコード → VK はドライバ内の固定表（コード 0x00〜0x6F、scanToVK）。
//
// モデル: キー操作をバイト列としてキューに積み、1 バイト渡すごとに（まだ
// 残っていれば）EINT1 を上げ直す。まとめて上げると INTC の SRCPND で 1 回に
// 潰れて取りこぼすため、1 割り込み 1 バイトを守る。
//
// TODO: 未確認事項:
//   - 送るデータがないときに実機のマイコンが返す値（ここでは 0）
//   - チップセレクト（GPB6）を見ていない（GPIO が値保持スタブのため）
//   - 初期化時の 3 バイトコマンドの意味（LED・リピート設定等？）
type kbdMCU struct {
	out []byte // ドライバに返す（ドライバが受信する）バイト列

	// raise は EINT1 を上げる（machine が配線）。
	raise func()
	// log はドライバが送ったバイト（調査用。状態ではない）。
	log []byte
}

func (k *kbdMCU) Transfer(tx byte) byte {
	k.log = append(k.log, tx)
	if len(k.out) == 0 {
		return 0
	}
	b := k.out[0]
	k.out = k.out[1:]
	if len(k.out) > 0 {
		k.raise() // 次のバイトの割り込み（IST の InterruptDone 後に受理される）
	}
	return b
}

func (k *kbdMCU) push(b byte) {
	k.out = append(k.out, b)
	if len(k.out) == 1 {
		k.raise()
	}
}

func (k *kbdMCU) StateVersion() uint16 { return 1 }

func (k *kbdMCU) SaveState(e *snapshot.Encoder) { e.Bytes(k.out) }

func (k *kbdMCU) LoadState(d *snapshot.Decoder) {
	if !d.CheckVersion(1) {
		return
	}
	k.out = d.Bytes()
}

// scanToVK は kbdmouse.dll のスキャンコード → VK の表（2026-09 に実行中の
// メモリから読み出した）のうち、キー名を付けて公開するもの。
//
// ソフトキー（WM5 の VK_TSOFT1/2 = VK_F1/VK_F2）は表に無く、この経路では
// 押せない。画面下端のソフトキー表示をタップすれば同じ操作になる。
// TODO: Device Emulator の本体ボタン（ソフトキー・電源等）が別経路か確認する。
var keyScanCodes = map[string]byte{
	// 方向キーと決定
	"Up": 0x6C, "Down": 0x6A, "Left": 0x6D, "Right": 0x6F, "Enter": 0x5A,
	// アプリケーションボタン（VK_APP1〜5）
	"App1": 0x64, "App2": 0x65, "App3": 0x66, "App4": 0x67, "App5": 0x68,
	// 編集キー
	"Space": 0x6E, "Back": 0x69, "Tab": 0x0B, "Esc": 0x29, "Delete": 0x2A,
	"Shift": 0x12, "RShift": 0x62, "Ctrl": 0x19, "Alt": 0x01, "CapsLock": 0x2C, "Win": 0x60,
	// 英字
	"A": 0x0D, "B": 0x3E, "C": 0x2E, "D": 0x35, "E": 0x3B, "F": 0x3D, "G": 0x45,
	"H": 0x4D, "I": 0x52, "J": 0x55, "K": 0x44, "L": 0x4C, "M": 0x4E, "N": 0x46,
	"O": 0x4B, "P": 0x53, "Q": 0x2B, "R": 0x43, "S": 0x2D, "T": 0x3A, "U": 0x4A,
	"V": 0x36, "W": 0x33, "X": 0x0E, "Y": 0x42, "Z": 0x0C,
	// 数字
	"0": 0x49, "1": 0x31, "2": 0x39, "3": 0x2F, "4": 0x37, "5": 0x3F,
	"6": 0x47, "7": 0x4F, "8": 0x57, "9": 0x41,
}

// KeyNames はキー名の一覧（ソート済み）。
func KeyNames() []string {
	names := make([]string, 0, len(keyScanCodes))
	for n := range keyScanCodes {
		names = append(names, n)
	}
	sort.Strings(names)
	return names
}

// KeyNames は machine.Machine のキー名一覧（パッケージ関数と同じ）。
func (m *Machine) KeyNames() []string { return KeyNames() }

// ValidKey はキー名が使えるか。
func ValidKey(name string) bool {
	_, ok := keyScanCodes[name]
	return ok
}

// KeyDown はキーを押す。
func (m *Machine) KeyDown(name string) error {
	sc, ok := keyScanCodes[name]
	if !ok {
		return fmt.Errorf("unknown key %q", name)
	}
	m.kbd.push(sc)
	return nil
}

// KeyUp はキーを離す。
func (m *Machine) KeyUp(name string) error {
	sc, ok := keyScanCodes[name]
	if !ok {
		return fmt.Errorf("unknown key %q", name)
	}
	m.kbd.push(sc | 0x80)
	return nil
}

// KeyboardRaw はキーボードマイコンから送るバイト列を直接積む（調査用）。
func (m *Machine) KeyboardRaw(b ...byte) {
	for _, x := range b {
		m.kbd.push(x)
	}
}

// KeyboardLog はドライバが送ったバイトを返して記録を空にする（調査用）。
func (m *Machine) KeyboardLog() []byte {
	l := m.kbd.log
	m.kbd.log = nil
	return l
}
