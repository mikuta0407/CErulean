package s3c2410

import (
	"time"

	"github.com/mikuta0407/cerulean/bus"
)

// RTC は S3C2410 のリアルタイムクロック（データシート Ch.17）の時刻部分。
//
// 時刻は「基準時刻 + 仮想時間の経過」で表す。仮想時間は machine が Advance で
// 渡す（命令数ベースで決定論的。ホストの時計は基準時刻の決定にしか使わない）。
// 基準時刻は machine が SetTime で与える（CLI は既定でホストの現在時刻）。
//
// レジスタ（BCD、各 8 ビット）:
//
//	0x40 RTCCON  bit0 RTCEN（1 の間だけ BCD レジスタに書ける）
//	0x70 BCDSEC  0x74 BCDMIN  0x78 BCDHOUR  0x7C BCDDATE
//	0x80 BCDDAY  0x84 BCDMON  0x88 BCDYEAR（00〜99 → 2000〜2099 とする）
//
// 実イメージの OAL は SEC→YEAR→MON→DATE→DAY→HOUR→MIN→SEC の順に読み、
// 最初と最後の秒が違えば読み直す（データシート推奨の桁上がり対策。
// 2026-09 に実測）。書き込み・RTCCON 操作は観測されていない。
//
// 未実装: アラーム（RTCALM 等）・ティック割り込み（TICNT）・RTCRST。
// これらは値保持のみ。
// レジスタオフセット（リトルエンディアン時）と BCDDAY の値域 1〜7 は
// User's Manual Rev 1.1 で確認済み。どの曜日を 1 とするかはマニュアルに
// 記載がない（ソフトウェアの約束事）。
// TODO: ここでは 1=日曜 としている。OAL が曜日を使う場面が出たら確認する。
type RTC struct {
	base    time.Time // 仮想時間 0 に対応する時刻（UTC として扱う壁時計の値）
	elapsed int64     // 経過した PCLK ティック
	pclkHz  int64

	rtccon uint32
	other  *Stub // アラーム等の値保持
}

var _ bus.Device = (*RTC)(nil)

const (
	regRTCCON  = 0x40
	regBCDSEC  = 0x70
	regBCDMIN  = 0x74
	regBCDHOUR = 0x78
	regBCDDATE = 0x7C
	regBCDDAY  = 0x80
	regBCDMON  = 0x84
	regBCDYEAR = 0x88
)

// NewRTC は pclkHz（仮想時間の 1 秒あたりの PCLK ティック数）で時間を進める
// RTC を作る。初期時刻は 2000-01-01 00:00:00。
func NewRTC(pclkHz int64) *RTC {
	return &RTC{
		base:   time.Date(2000, 1, 1, 0, 0, 0, 0, time.UTC),
		pclkHz: pclkHz,
		other:  NewStub("rtc", nil),
	}
}

// SetTime は現在時刻を t にする。t のタイムゾーンでの壁時計の値（年月日時分秒）
// をそのまま RTC の値として使う（WinCE の RTC はローカル時刻を持つ）。
func (r *RTC) SetTime(t time.Time) {
	r.base = time.Date(t.Year(), t.Month(), t.Day(), t.Hour(), t.Minute(), t.Second(), 0, time.UTC)
	r.elapsed = 0
}

// Advance は仮想時間を PCLK ティック数だけ進める。
func (r *RTC) Advance(ticks int64) { r.elapsed += ticks }

// Now は現在の RTC 時刻。
func (r *RTC) Now() time.Time {
	return r.base.Add(time.Duration(r.elapsed / r.pclkHz * int64(time.Second)))
}

func toBCD(v int) uint32   { return uint32(v/10)<<4 | uint32(v%10) }
func fromBCD(v uint32) int { return int(v>>4&0xF)*10 + int(v&0xF) }

func (r *RTC) Read(off uint32, size int) uint32 {
	t := r.Now()
	switch off &^ 3 {
	case regRTCCON:
		return r.rtccon
	case regBCDSEC:
		return toBCD(t.Second())
	case regBCDMIN:
		return toBCD(t.Minute())
	case regBCDHOUR:
		return toBCD(t.Hour())
	case regBCDDATE:
		return toBCD(t.Day())
	case regBCDDAY:
		return toBCD(int(t.Weekday()) + 1)
	case regBCDMON:
		return toBCD(int(t.Month()))
	case regBCDYEAR:
		return toBCD(t.Year() % 100)
	}
	return r.other.Read(off, size)
}

func (r *RTC) Write(off uint32, size int, v uint32) {
	off &^= 3
	if off == regRTCCON {
		r.rtccon = v
		return
	}
	if off < regBCDSEC || off > regBCDYEAR {
		r.other.Write(off, size, v)
		return
	}
	if r.rtccon&1 == 0 {
		return // RTCEN=0 の間は時刻レジスタは書き込み不可
	}
	t := r.Now()
	y, mo, d, h, mi, s := t.Year(), int(t.Month()), t.Day(), t.Hour(), t.Minute(), t.Second()
	n := fromBCD(v & 0xFF)
	switch off {
	case regBCDSEC:
		s = n
	case regBCDMIN:
		mi = n
	case regBCDHOUR:
		h = n
	case regBCDDATE:
		d = n
	case regBCDDAY:
		return // 曜日は日付から求めるので書き込みは捨てる（TODO: 独立カウンタか要確認）
	case regBCDMON:
		mo = n
	case regBCDYEAR:
		y = 2000 + n
	}
	// 秒未満の経過（ティック端数）は保ったまま、壁時計の値だけ置き換える。
	frac := r.elapsed % r.pclkHz
	r.base = time.Date(y, time.Month(mo), d, h, mi, s, 0, time.UTC)
	r.elapsed = frac
}
