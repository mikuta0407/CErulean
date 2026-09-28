//! リアルタイムクロック（データシート Ch.17）の時刻部分。

use super::Stub;

/// S3C2410 の RTC。
///
/// 時刻は「基準時刻 + 仮想時間の経過」で表す。仮想時間は machine が advance で
/// 渡す（命令数ベースで決定論的。ホストの時計は基準時刻の決定にしか使わない）。
/// 基準時刻は machine が set_time で与える（フロントエンドがホストのローカル
/// 時刻の年月日時分秒を渡す）。
///
/// 基準時刻は「UTC として扱う壁時計の値」を 1970-01-01 00:00:00 からの秒数で
/// 持ち、読み出し時に 基準 + ⌊経過ティック ÷ pclk_hz⌋ 秒 から年月日・曜日を
/// 計算する（夏時間は関係しない。Go の time.Time を UTC で使っていたのと同じ）。
///
/// レジスタ（BCD、各 8 ビット）:
///
/// ```text
/// 0x40 RTCCON  bit0 RTCEN（1 の間だけ BCD レジスタに書ける）
/// 0x70 BCDSEC  0x74 BCDMIN  0x78 BCDHOUR  0x7C BCDDATE
/// 0x80 BCDDAY  0x84 BCDMON  0x88 BCDYEAR（00〜99 → 2000〜2099 とする）
/// ```
///
/// 実イメージの OAL は SEC→YEAR→MON→DATE→DAY→HOUR→MIN→SEC の順に読み、
/// 最初と最後の秒が違えば読み直す（データシート推奨の桁上がり対策。
/// 2026-09 に実測）。書き込み・RTCCON 操作は観測されていない。
///
/// 未実装: アラーム（RTCALM 等）・ティック割り込み（TICNT）・RTCRST。
/// これらは値保持のみ。レジスタオフセットと BCDDAY の値域 1〜7 は
/// User's Manual Rev 1.1 で確認済み。どの曜日を 1 とするかはマニュアルに
/// 記載がない（ソフトウェアの約束事）。
/// TODO: ここでは 1=日曜 としている。OAL が曜日を使う場面が出たら確認する。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rtc {
    /// 仮想時間 0 に対応する時刻（1970-01-01 00:00:00 からの秒。UTC として扱う壁時計の値）
    pub(crate) base: i64,
    /// 経過した PCLK ティック
    pub(crate) elapsed: i64,
    /// 仮想時間の 1 秒あたりの PCLK ティック数（構成で決まる）
    pclk_hz: i64,
    pub(crate) rtccon: u32,
    /// アラーム等の値保持
    pub(crate) other: Stub,
}

const REG_RTCCON: u32 = 0x40;
const REG_BCDSEC: u32 = 0x70;
const REG_BCDMIN: u32 = 0x74;
const REG_BCDHOUR: u32 = 0x78;
const REG_BCDDATE: u32 = 0x7C;
const REG_BCDDAY: u32 = 0x80;
const REG_BCDMON: u32 = 0x84;
const REG_BCDYEAR: u32 = 0x88;

/// 暦の日時（年月日時分秒と曜日。0=日曜）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DateTime {
    pub year: i64,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
    pub weekday: u32,
}

impl Rtc {
    /// pclk_hz（仮想時間の 1 秒あたりの PCLK ティック数）で時間を進める RTC。
    /// 初期時刻は 2000-01-01 00:00:00。
    pub fn new(pclk_hz: i64) -> Rtc {
        Rtc {
            base: date_to_unix(2000, 1, 1, 0, 0, 0),
            elapsed: 0,
            pclk_hz,
            rtccon: 0,
            other: Stub::new(&[]),
        }
    }

    /// 現在時刻を年月日時分秒にする（壁時計の値をそのまま RTC の値として使う。
    /// WinCE の RTC はローカル時刻を持つ）。範囲外の値は Go の time.Date と同じく
    /// 繰り上げて正規化する。
    pub fn set_time(
        &mut self,
        year: i64,
        month: i64,
        day: i64,
        hour: i64,
        minute: i64,
        second: i64,
    ) {
        self.base = date_to_unix(year, month, day, hour, minute, second);
        self.elapsed = 0;
    }

    /// 仮想時間を PCLK ティック数だけ進める。
    pub fn advance(&mut self, ticks: i64) {
        self.elapsed += ticks;
    }

    /// 現在の RTC 時刻。
    pub fn now(&self) -> DateTime {
        unix_to_date(self.base + self.elapsed / self.pclk_hz)
    }

    pub fn read(&self, off: u32, size: u32) -> u32 {
        let t = self.now();
        match off & !3 {
            REG_RTCCON => self.rtccon,
            REG_BCDSEC => to_bcd(t.second),
            REG_BCDMIN => to_bcd(t.minute),
            REG_BCDHOUR => to_bcd(t.hour),
            REG_BCDDATE => to_bcd(t.day),
            REG_BCDDAY => to_bcd(t.weekday + 1),
            REG_BCDMON => to_bcd(t.month),
            REG_BCDYEAR => to_bcd(t.year.rem_euclid(100) as u32),
            _ => self.other.read(off, size),
        }
    }

    pub fn write(&mut self, off: u32, size: u32, v: u32) {
        let off = off & !3;
        if off == REG_RTCCON {
            self.rtccon = v;
            return;
        }
        if !(REG_BCDSEC..=REG_BCDYEAR).contains(&off) {
            self.other.write(off, size, v);
            return;
        }
        if self.rtccon & 1 == 0 {
            return; // RTCEN=0 の間は時刻レジスタは書き込み不可
        }
        let t = self.now();
        let (mut y, mut mo, mut d, mut h, mut mi, mut s) = (
            t.year,
            t.month as i64,
            t.day as i64,
            t.hour as i64,
            t.minute as i64,
            t.second as i64,
        );
        let n = from_bcd(v & 0xFF) as i64;
        match off {
            REG_BCDSEC => s = n,
            REG_BCDMIN => mi = n,
            REG_BCDHOUR => h = n,
            REG_BCDDATE => d = n,
            // 曜日は日付から求めるので書き込みは捨てる（TODO: 独立カウンタか要確認）
            REG_BCDDAY => return,
            REG_BCDMON => mo = n,
            _ => y = 2000 + n, // BCDYEAR
        }
        // 秒未満の経過（ティック端数）は保ったまま、壁時計の値だけ置き換える。
        let frac = self.elapsed % self.pclk_hz;
        self.base = date_to_unix(y, mo, d, h, mi, s);
        self.elapsed = frac;
    }
}

pub(crate) fn to_bcd(v: u32) -> u32 {
    ((v / 10) << 4) | (v % 10)
}

/// 不正な BCD（桁が 10 以上）もそのまま計算する（Go の fromBCD と同じ。0〜165）。
pub(crate) fn from_bcd(v: u32) -> u32 {
    ((v >> 4) & 0xF) * 10 + (v & 0xF)
}

// ---- グレゴリオ暦の計算（Rust の標準ライブラリには暦がないので自前）----

/// 年月日の 1970-01-01 からの日数（先発グレゴリオ暦。月は 1〜12）。
/// 3 月始まりの年に直して 400 年周期で数える、よく知られた変換式。
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400; // 0..=399
    let mp = (m + 9) % 12; // 3 月 = 0
    let doy = (153 * mp + 2) / 5 + d - 1; // 0..=365
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // 0..=146096
    era * 146097 + doe - 719468
}

/// days_from_civil の逆。
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    (y, m, d)
}

/// Go の time.Date(y, mo, d, h, mi, s, 0, UTC).Unix() と同じ値。
/// Go は月を年に繰り上げて正規化してから、日・時・分・秒は線形に足す
/// （2 月 31 日 = 3 月 3 日 など）。ここでも同じ順に計算する。
pub(crate) fn date_to_unix(y: i64, mo: i64, d: i64, h: i64, mi: i64, s: i64) -> i64 {
    let m0 = mo - 1;
    let (y, m0) = (y + m0.div_euclid(12), m0.rem_euclid(12));
    let days = days_from_civil(y, m0 + 1, 1) + (d - 1);
    days * 86400 + h * 3600 + mi * 60 + s
}

/// 1970-01-01 からの秒を暦に直す（曜日は 0=日曜。1970-01-01 は木曜）。
pub(crate) fn unix_to_date(t: i64) -> DateTime {
    let days = t.div_euclid(86400);
    let sec = t.rem_euclid(86400) as u32;
    let (year, month, day) = civil_from_days(days);
    DateTime {
        year,
        month,
        day,
        hour: sec / 3600,
        minute: sec / 60 % 60,
        second: sec % 60,
        weekday: (days + 4).rem_euclid(7) as u32,
    }
}
