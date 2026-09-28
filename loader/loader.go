// Package loader は Windows CE のカーネルイメージ（nk.bin 等）を読み込み、
// 形式に依存しない中間表現 Image に変換する。
//
// 対応形式:
//   - Windows CE BIN 形式（"B000FF" 署名 + レコード列）: Platform Builder の出力
//   - .nb0 生形式（ヘッダなしのフラットイメージ）: WM5 Device Emulator 用イメージで
//     使われている可能性がある形式
//
// TODO: 実際の WM5 Device Emulator イメージ（英語版/日本語版）を入手して、
// どちらの形式か・エントリポイントの規則を確認する。
package loader

import (
	"bytes"
	"fmt"
	"io"
	"os"
	"strings"
)

// Segment はイメージ内の連続したデータ片。
// Addr はイメージファイルに記載されたアドレスで、通常は CE カーネルの
// 仮想アドレス（0x80000000 台）。物理アドレスへの変換は machine 側の責務。
type Segment struct {
	Addr uint32
	Data []byte
}

// Image は形式共通の中間表現。
type Image struct {
	Format string    // "bin", "nb0" or "words"
	Start  uint32    // イメージ全体の開始アドレス
	Length uint32    // イメージ全体の長さ（バイト）
	Entry  uint32    // エントリポイント
	Segs   []Segment // ロードすべきデータ片（アドレス順とは限らない）
}

// Load はパスからイメージを読み込む。形式はファイル先頭のマジックで判別し、
// マジックがなければ拡張子 .nb0 のとき生形式、.words のとき命令語の
// テキスト（LoadWords）として扱う。
// nb0Base は .nb0 のときのロード先アドレス（BIN 形式では無視される）。
func Load(path string, nb0Base uint32) (*Image, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		return nil, err
	}
	if bytes.HasPrefix(data, binMagic) {
		bimg, err := LoadBIN(bytes.NewReader(data))
		if err != nil {
			return nil, err
		}
		return &bimg.Image, nil
	}
	if strings.HasSuffix(strings.ToLower(path), ".nb0") {
		return LoadNB0(bytes.NewReader(data), nb0Base)
	}
	if strings.HasSuffix(strings.ToLower(path), ".words") {
		return LoadWords(bytes.NewReader(data))
	}
	return nil, fmt.Errorf("loader: %s: unknown image format (no B000FF magic and not .nb0/.words)", path)
}

// LoadNB0 はヘッダなしの生イメージを読み込む。base にそのまま配置され、
// エントリポイントは先頭アドレスと仮定する。
// TODO: 実イメージで「エントリ = 先頭」の仮定を確認する（イメージ先頭が
// スタートアップコードへのジャンプになっているのが通例のはずだが未検証）。
func LoadNB0(r io.Reader, base uint32) (*Image, error) {
	data, err := io.ReadAll(r)
	if err != nil {
		return nil, err
	}
	if len(data) == 0 {
		return nil, fmt.Errorf("loader: empty nb0 image")
	}
	return &Image{
		Format: "nb0",
		Start:  base,
		Length: uint32(len(data)),
		Entry:  base,
		Segs:   []Segment{{Addr: base, Data: data}},
	}, nil
}
