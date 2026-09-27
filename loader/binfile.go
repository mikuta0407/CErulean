package loader

import (
	"bufio"
	"encoding/binary"
	"fmt"
	"io"
)

// Windows CE BIN 形式（.bin / nk.bin）:
//
//	署名     "B000FF\x0A"（7 バイト）
//	ヘッダ   start uint32LE, length uint32LE  … イメージ全体の範囲
//	レコード addr uint32LE, len uint32LE, checksum uint32LE, data[len]
//	終端     addr == 0 のレコード。len フィールドがエントリポイント
//	         （checksum は 0、データなし）。実イメージ（WM5 SDK の
//	         PPC_USA.bin）で確認済みの規則。
//
// checksum は data の全バイトの単純和（uint32、桁あふれは無視）。
var binMagic = []byte("B000FF\x0A")

// BINRecord は BIN 形式の 1 レコード。Image.Segs に入れる Segment と同じ内容だが、
// info コマンドでチェックサム等を表示できるよう別途保持する。
type BINRecord struct {
	Addr     uint32
	Len      uint32
	Checksum uint32
}

// BINImage は BIN 形式固有の情報付き Image。
type BINImage struct {
	Image
	Records []BINRecord // 終端レコードは含まない
}

// LoadBIN は Windows CE BIN 形式を読み込む。チェックサム不一致はエラーにする
// （壊れたイメージを黙って実行しても原因究明が困難になるだけのため）。
func LoadBIN(r io.Reader) (*BINImage, error) {
	br := bufio.NewReader(r)

	magic := make([]byte, len(binMagic))
	if _, err := io.ReadFull(br, magic); err != nil {
		return nil, fmt.Errorf("loader: reading BIN magic: %w", err)
	}
	if string(magic) != string(binMagic) {
		return nil, fmt.Errorf("loader: bad BIN magic %q", magic)
	}

	var start, length uint32
	if err := readU32(br, &start); err != nil {
		return nil, fmt.Errorf("loader: reading image start: %w", err)
	}
	if err := readU32(br, &length); err != nil {
		return nil, fmt.Errorf("loader: reading image length: %w", err)
	}

	img := &BINImage{Image: Image{Format: "bin", Start: start, Length: length}}
	for i := 0; ; i++ {
		var addr, rlen, sum uint32
		if err := readU32(br, &addr); err != nil {
			return nil, fmt.Errorf("loader: record %d: reading addr: %w", i, err)
		}
		if err := readU32(br, &rlen); err != nil {
			return nil, fmt.Errorf("loader: record %d: reading len: %w", i, err)
		}
		if err := readU32(br, &sum); err != nil {
			return nil, fmt.Errorf("loader: record %d: reading checksum: %w", i, err)
		}
		if addr == 0 {
			// 終端レコード: len フィールドがエントリポイント。
			img.Entry = rlen
			return img, nil
		}
		if rlen == 0 {
			// データなしレコード。一部のツールは {addr=entry, len=0} を
			// 終端として書くという情報もあるため、同様に終端として扱う。
			img.Entry = addr
			return img, nil
		}
		data := make([]byte, rlen)
		if _, err := io.ReadFull(br, data); err != nil {
			return nil, fmt.Errorf("loader: record %d (addr=%08X len=%d): reading data: %w", i, addr, rlen, err)
		}
		if got := byteSum(data); got != sum {
			return nil, fmt.Errorf("loader: record %d (addr=%08X len=%d): checksum mismatch: file says %08X, computed %08X",
				i, addr, rlen, sum, got)
		}
		img.Records = append(img.Records, BINRecord{Addr: addr, Len: rlen, Checksum: sum})
		img.Segs = append(img.Segs, Segment{Addr: addr, Data: data})
	}
}

// byteSum は BIN 形式のチェックサム（データ全バイトの和、mod 2^32）。
func byteSum(data []byte) uint32 {
	var s uint32
	for _, b := range data {
		s += uint32(b)
	}
	return s
}

func readU32(r io.Reader, v *uint32) error {
	var buf [4]byte
	if _, err := io.ReadFull(r, buf[:]); err != nil {
		return err
	}
	*v = binary.LittleEndian.Uint32(buf[:])
	return nil
}
