// Package snapshot はエミュレータ全状態の保存形式（コンテナとエンコーダ）。
//
// SoC・デバイス固有の知識は持たない。各コンポーネントが Encoder/Decoder で
// 自分の状態を読み書きし、どのコンポーネントをどの順で並べるかは machine が
// 決める（ユーザー確認済みの方針 2026-09）。
//
// ファイル形式:
//
//	"CERUSNAP" | FormatVersion(u32 LE) | flate 圧縮ストリーム
//
// 圧縮ストリームの中身:
//
//	ヘッダ: machine 名(string) | imageID(string)
//	チャンク × n: 名前(string) | 版数(u16) | 本体 | 本体バイト数(u64)
//	終端: 空文字列の名前
//
// 設計判断:
//   - チャンクはコンポーネント単位で、版数を独立に持つ（あるデバイスの状態が
//     増えても、そのデバイスの版数だけ上げればよい）。
//   - 本体の長さは先頭でなく末尾（トレーラ）に置く。RAM（128MB）を一度
//     メモリ上に組み立てずにストリームで書けるようにするため。読み手は
//     消費したバイト数とトレーラを照合し、書き手と読み手の食い違い
//     （フィールドの追加漏れ等）を検出する。
//   - 未知のチャンク・順序違い・版数の非対応は黙って読み飛ばさずエラーにする
//     （中途半端な復元はデバッグ不能な非決定性の原因になるため）。
//   - 全体を flate（標準ライブラリ、純 Go）で圧縮する。RAM の大半は 0 なので
//     よく縮む。速度優先で BestSpeed。
//   - 数値はリトルエンディアン固定。
package snapshot

import (
	"bufio"
	"compress/flate"
	"encoding/binary"
	"errors"
	"fmt"
	"io"
)

// Magic はファイル先頭の識別子。
const Magic = "CERUSNAP"

// FormatVersion はコンテナ形式（ヘッダ・チャンク枠）の版数。
// チャンクの中身の変更ではなく、枠組みを変えたときだけ上げる。
const FormatVersion = 1

// Stateful はスナップショットに状態を保存できるコンポーネント。
// 配線（コールバック・io.Writer 等）や設定値は状態ではないので保存しない。
// それらは New で組み立て済みのオブジェクトに LoadState で状態だけを戻す。
type Stateful interface {
	// StateVersion はこのコンポーネントの状態形式の版数。
	StateVersion() uint16
	// SaveState は状態を e に書く。
	SaveState(e *Encoder)
	// LoadState は d から状態を戻す。d.Version() は保存時の版数。
	// エラーは d.Fail で報告する（呼び出し側が d.Err で検査する）。
	LoadState(d *Decoder)
}

// Writer はスナップショットを書き出す。
type Writer struct {
	fw  *flate.Writer
	bw  *bufio.Writer
	err error
}

// NewWriter はヘッダを書いて Writer を返す。machineName は復元先の検証用、
// imageID は元イメージの識別子（呼び出し側が決める。例: SHA-256 の 16 進）。
func NewWriter(w io.Writer, machineName, imageID string) (*Writer, error) {
	var hdr [len(Magic) + 4]byte
	copy(hdr[:], Magic)
	binary.LittleEndian.PutUint32(hdr[len(Magic):], FormatVersion)
	if _, err := w.Write(hdr[:]); err != nil {
		return nil, err
	}
	fw, err := flate.NewWriter(w, flate.BestSpeed)
	if err != nil {
		return nil, err
	}
	sw := &Writer{fw: fw, bw: bufio.NewWriterSize(fw, 64*1024)}
	e := &Encoder{w: sw.bw}
	e.String(machineName)
	e.String(imageID)
	sw.err = e.err
	return sw, sw.err
}

// Chunk は名前 name のチャンクとして s の状態を書く。
func (w *Writer) Chunk(name string, s Stateful) error {
	if w.err != nil {
		return w.err
	}
	if name == "" {
		return errors.New("snapshot: empty chunk name")
	}
	e := &Encoder{w: w.bw}
	e.String(name)
	e.U16(s.StateVersion())
	e.n = 0 // 本体バイト数はここから数える
	s.SaveState(e)
	body := e.n
	e.U64(body)
	w.err = e.err
	return w.err
}

// Close は終端を書いて圧縮ストリームを閉じる（下の io.Writer は閉じない）。
func (w *Writer) Close() error {
	if w.err != nil {
		return w.err
	}
	e := &Encoder{w: w.bw}
	e.String("")
	if e.err != nil {
		return e.err
	}
	if err := w.bw.Flush(); err != nil {
		return err
	}
	return w.fw.Close()
}

// Header はスナップショットのヘッダ情報。
type Header struct {
	Machine string
	ImageID string
}

// Reader はスナップショットを読む。チャンクは書かれた順に読む。
type Reader struct {
	Header Header
	br     *bufio.Reader
	fr     io.ReadCloser
}

// NewReader は識別子・版数・ヘッダを検証して Reader を返す。
func NewReader(r io.Reader) (*Reader, error) {
	var hdr [len(Magic) + 4]byte
	if _, err := io.ReadFull(r, hdr[:]); err != nil {
		return nil, fmt.Errorf("snapshot: reading header: %w", err)
	}
	if string(hdr[:len(Magic)]) != Magic {
		return nil, errors.New("snapshot: not a CErulean snapshot (bad magic)")
	}
	if v := binary.LittleEndian.Uint32(hdr[len(Magic):]); v != FormatVersion {
		return nil, fmt.Errorf("snapshot: unsupported format version %d (want %d)", v, FormatVersion)
	}
	fr := flate.NewReader(r)
	sr := &Reader{fr: fr, br: bufio.NewReaderSize(fr, 64*1024)}
	d := &Decoder{r: sr.br}
	sr.Header.Machine = d.String()
	sr.Header.ImageID = d.String()
	if d.err != nil {
		return nil, fmt.Errorf("snapshot: reading header: %w", d.err)
	}
	return sr, nil
}

// Chunk は次のチャンクが name であることを確かめ、s に状態を戻す。
func (r *Reader) Chunk(name string, s Stateful) error {
	d := &Decoder{r: r.br}
	got := d.String()
	ver := d.U16()
	if d.err != nil {
		return fmt.Errorf("snapshot: chunk %q: %w", name, d.err)
	}
	if got != name {
		return fmt.Errorf("snapshot: expected chunk %q, found %q", name, got)
	}
	d.version = ver
	d.n = 0
	s.LoadState(d)
	consumed := d.n
	trailer := d.U64()
	if d.err != nil {
		return fmt.Errorf("snapshot: chunk %q (version %d): %w", name, ver, d.err)
	}
	if trailer != consumed {
		return fmt.Errorf("snapshot: chunk %q (version %d): read %d bytes but chunk has %d (writer/reader mismatch)",
			name, ver, consumed, trailer)
	}
	return nil
}

// Close は終端を確認する（余分なチャンクが残っていればエラー）。
func (r *Reader) Close() error {
	d := &Decoder{r: r.br}
	name := d.String()
	if d.err != nil {
		return fmt.Errorf("snapshot: reading end marker: %w", d.err)
	}
	if name != "" {
		return fmt.Errorf("snapshot: unexpected extra chunk %q", name)
	}
	return r.fr.Close()
}

// Encoder はチャンク本体の書き込み。最初のエラーを保持し、以降は何もしない
// （呼び出し側で 1 フィールドごとに err を見なくて済むように）。
type Encoder struct {
	w   io.Writer
	n   uint64 // 書いたバイト数
	err error
	buf [8]byte
}

func (e *Encoder) write(p []byte) {
	if e.err != nil {
		return
	}
	n, err := e.w.Write(p)
	e.n += uint64(n)
	e.err = err
}

func (e *Encoder) U8(v uint8) { e.buf[0] = v; e.write(e.buf[:1]) }
func (e *Encoder) U16(v uint16) {
	binary.LittleEndian.PutUint16(e.buf[:], v)
	e.write(e.buf[:2])
}
func (e *Encoder) U32(v uint32) {
	binary.LittleEndian.PutUint32(e.buf[:], v)
	e.write(e.buf[:4])
}
func (e *Encoder) U64(v uint64) {
	binary.LittleEndian.PutUint64(e.buf[:], v)
	e.write(e.buf[:8])
}
func (e *Encoder) I64(v int64) { e.U64(uint64(v)) }
func (e *Encoder) Bool(v bool) {
	if v {
		e.U8(1)
	} else {
		e.U8(0)
	}
}

// Bytes は長さ（u64）付きでバイト列を書く。
func (e *Encoder) Bytes(p []byte) {
	e.U64(uint64(len(p)))
	e.write(p)
}

func (e *Encoder) String(s string) { e.Bytes([]byte(s)) }

// U32s は長さ付きで uint32 の列を書く。
func (e *Encoder) U32s(vs []uint32) {
	e.U64(uint64(len(vs)))
	for _, v := range vs {
		e.U32(v)
	}
}

// Decoder はチャンク本体の読み出し。Encoder と対称。最初のエラーを保持し、
// 以降の読み出しはゼロ値を返す。
type Decoder struct {
	r       io.Reader
	n       uint64
	version uint16
	err     error
	buf     [8]byte
}

// Version は保存時のチャンク版数。
func (d *Decoder) Version() uint16 { return d.version }

// Err は最初に起きたエラー。
func (d *Decoder) Err() error { return d.err }

// Fail は LoadState 側で検出した不整合を報告する。
func (d *Decoder) Fail(format string, args ...any) {
	if d.err == nil {
		d.err = fmt.Errorf(format, args...)
	}
}

// CheckVersion は保存時版数が want でなければ Fail する（非対応の旧形式を
// 黙って読まないため）。一致すれば true。
func (d *Decoder) CheckVersion(want uint16) bool {
	if d.version != want {
		d.Fail("unsupported state version %d (want %d)", d.version, want)
		return false
	}
	return d.err == nil
}

func (d *Decoder) read(p []byte) {
	if d.err != nil {
		for i := range p {
			p[i] = 0
		}
		return
	}
	n, err := io.ReadFull(d.r, p)
	d.n += uint64(n)
	if err != nil {
		if err == io.EOF {
			err = io.ErrUnexpectedEOF
		}
		d.err = err
	}
}

func (d *Decoder) U8() uint8 { d.read(d.buf[:1]); return d.buf[0] }
func (d *Decoder) U16() uint16 {
	d.read(d.buf[:2])
	return binary.LittleEndian.Uint16(d.buf[:])
}
func (d *Decoder) U32() uint32 {
	d.read(d.buf[:4])
	return binary.LittleEndian.Uint32(d.buf[:])
}
func (d *Decoder) U64() uint64 {
	d.read(d.buf[:8])
	return binary.LittleEndian.Uint64(d.buf[:])
}
func (d *Decoder) I64() int64 { return int64(d.U64()) }
func (d *Decoder) Bool() bool {
	switch v := d.U8(); v {
	case 0:
		return false
	case 1:
		return true
	default:
		d.Fail("invalid bool byte %d", v)
		return false
	}
}

// maxAlloc は長さ付きデータ 1 個の上限（壊れたファイルで巨大確保しないため）。
// RAM 領域（128MB）が入る大きさにしてある。
const maxAlloc = 1 << 30

func (d *Decoder) length() int {
	n := d.U64()
	if n > maxAlloc {
		d.Fail("length %d too large", n)
		return 0
	}
	return int(n)
}

// Bytes は長さ付きバイト列を新しいスライスに読む。
func (d *Decoder) Bytes() []byte {
	n := d.length()
	if d.err != nil {
		return nil
	}
	p := make([]byte, n)
	d.read(p)
	return p
}

// BytesInto は長さ付きバイト列を dst に読む。長さが dst と違えば Fail
// （RAM サイズなど構成が一致しないスナップショットを弾く）。
func (d *Decoder) BytesInto(dst []byte) {
	n := d.length()
	if d.err != nil {
		return
	}
	if n != len(dst) {
		d.Fail("byte length %d does not match destination %d", n, len(dst))
		return
	}
	d.read(dst)
}

func (d *Decoder) String() string { return string(d.Bytes()) }

// U32sInto は長さ付き uint32 列を dst に読む。長さ不一致は Fail。
func (d *Decoder) U32sInto(dst []uint32) {
	n := d.length()
	if d.err != nil {
		return
	}
	if n != len(dst) {
		d.Fail("uint32 array length %d does not match destination %d", n, len(dst))
		return
	}
	for i := range dst {
		dst[i] = d.U32()
	}
}
