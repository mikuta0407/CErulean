//! A/D コンバータとタッチスクリーン I/F（データシート Ch.16）。

use super::{NO_EVENT, SUB_ADC, SUB_TC};

/// S3C2410 の A/D コンバータとタッチスクリーン I/F。
///
/// レジスタ（オフセット）:
///
/// ```text
/// 0x00 ADCCON  ECFLG[15](RO) PRSCEN[14] PRSCVL[13:6] SEL_MUX[5:3]
///              STDBM[2] READ_START[1] ENABLE_START[0]
/// 0x04 ADCTSC  UD_SEN[8] YM_SEN[7] YP_SEN[6] XM_SEN[5] XP_SEN[4] PULL_UP[3]
///              AUTO_PST[2] XY_PST[1:0]（00 なし/01 X 測定/10 Y 測定/11 割り込み待ち）
/// 0x08 ADCDLY
/// 0x0C ADCDAT0 UPDOWN[15](0=ペンダウン) AUTO_PST[14] XY_PST[13:12] XPDATA[9:0]
/// 0x10 ADCDAT1 UPDOWN[15] AUTO_PST[14] XY_PST[13:12] YPDATA[9:0]
/// ```
///
/// 割り込み: INT_ADC のサブソース INT_TC（ペン検出）と INT_ADC（変換完了）。
/// 上げたサブソースは戻り値（SUBSRCPND のビットの束）でボードに返す。
///
/// モデル:
///   - ペンの状態と「その位置での X/Y の ADC 生値（10 ビット）」は machine が
///     set_pen で与える。画面座標→生値の変換は machine の責務（パネルの
///     向き・範囲はボード依存のため）。
///   - 変換は開始から conversion_ticks 後に完了し、ECFLG を立てて INT_ADC を
///     上げる。AUTO_PST=1 なら X・Y を両方、XY_PST=01/10 なら片方を測る。
///   - 割り込み待ちモード（XY_PST=11）で、UD_SEN=0 ならペンダウン、UD_SEN=1 なら
///     ペンアップを検出して INT_TC を上げる。
///
/// UD_SEN（bit8）について: S3C2410 のマニュアルでは bit8 は「予約（0 にすること）」
/// で、UD_SEN は S3C2440 の機能。しかし実イメージの touch.dll（Device Emulator 用）
/// は、サンプリング後に ADCTSC=0x1D3（bit8=1 の割り込み待ち）を書き、ペンアップを
/// INT_TC だけで検出する（タイマー駆動のサンプリング経路は UPDOWN を一切読まない。
/// 2026-09 にコードをトレースして確認）。Device Emulator の ADC は bit8 を S3C2440 の
/// UD_SEN と同じ意味で実装していると判断し、それに合わせる。
///
/// 根拠: User's Manual Rev 1.1 の Ch.16（レジスタ配置・リセット値・変換時間・
/// ADCDLY は確認済み）。
///
/// 変換時間: 1 回の測定 = ADCDLY の遅延（変換中は PCLK で数える。Figure 16-3）
/// ＋ 5 ADC クロック（ADC クロック = PCLK/(PRSCVL+1)。「50MHz/(49+1)=1MHz、
/// 5 サイクルで 5us」の記述）。自動（逐次）モードは X と Y の 2 回分。
///
/// TODO: 未確認・未実装:
///   - プリスケーラ無効（PRSCEN=0）時の ADC クロック（ここでは PCLK として 5 PCLK）
///   - 割り込み待ちモードでペンダウン中に ADCDLY 間隔で INT_TC を繰り返す動作
///     （ADCDLY の説明にある。touch.dll は Timer3 でサンプリングするので未実装）
///   - 割り込み待ちに入った時点で既に検出対象の状態なら INT_TC を出す（レベル扱い）
///     のは判断。こうしないと、サンプリング中（ADCTSC=0xDC の間）にペンが上がった
///     場合にペンアップを取りこぼす。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Adc {
    pub(crate) adccon: u32,
    pub(crate) adctsc: u32,
    pub(crate) adcdly: u32,
    /// 変換結果（下位 10 ビット）
    pub(crate) dat0: u32,
    pub(crate) dat1: u32,
    /// 変換完了までの残り PCLK ティック（0 = 変換中でない）
    pub(crate) converting: i64,
    pub(crate) ecflg: bool,
    pub(crate) pen_down: bool,
    pub(crate) raw_x: u32,
    pub(crate) raw_y: u32,
}

pub(crate) const REG_ADCCON: u32 = 0x00;
pub(crate) const REG_ADCTSC: u32 = 0x04;
pub(crate) const REG_ADCDLY: u32 = 0x08;
pub(crate) const REG_ADCDAT0: u32 = 0x0C;
pub(crate) const REG_ADCDAT1: u32 = 0x10;

pub(crate) const ADCCON_ECFLG: u32 = 1 << 15;
pub(crate) const ADCCON_PRSCEN: u32 = 1 << 14;
pub(crate) const ADCCON_READSTART: u32 = 1 << 1;
pub(crate) const ADCCON_ENABLESTART: u32 = 1 << 0;

const ADCTSC_AUTO_PST: u32 = 1 << 2;
const ADCTSC_UD_SEN: u32 = 1 << 8;
const XY_PST_X: u32 = 1;
const XY_PST_Y: u32 = 2;
const XY_PST_WAIT: u32 = 3;

pub(crate) const DAT_UPDOWN: u32 = 1 << 15;

impl Default for Adc {
    fn default() -> Self {
        Self::new()
    }
}

impl Adc {
    pub fn new() -> Adc {
        Adc {
            adccon: 0x3FC4, // リセット値（マニュアルで確認済み）
            adctsc: 0x58,
            adcdly: 0xFF,
            dat0: 0,
            dat1: 0,
            converting: 0,
            ecflg: false,
            pen_down: false,
            raw_x: 0,
            raw_y: 0,
        }
    }

    fn xy_pst(&self) -> u32 {
        self.adctsc & 3
    }

    /// ペンの状態と、その位置で X/Y を測ったときの生値（0〜1023）を設定する。
    /// 割り込み待ちモードで検出対象の変化（UD_SEN 参照）が起きたら INT_TC を
    /// 上げる。戻り値は上げたサブソースのビットの束。
    pub fn set_pen(&mut self, down: bool, raw_x: u32, raw_y: u32) -> u32 {
        let was = self.pen_down;
        self.pen_down = down;
        self.raw_x = raw_x & 0x3FF;
        self.raw_y = raw_y & 0x3FF;
        if down != was && self.tc_condition() {
            1 << SUB_TC
        } else {
            0
        }
    }

    /// 「割り込み待ちモードで、検出対象のペン状態にある」か。
    fn tc_condition(&self) -> bool {
        if self.xy_pst() != XY_PST_WAIT {
            return false;
        }
        let want_up = self.adctsc & ADCTSC_UD_SEN != 0;
        self.pen_down != want_up
    }

    /// 変換開始から完了までの PCLK ティック数（型のコメント参照）。
    fn conversion_ticks(&self) -> i64 {
        let conv = if self.adccon & ADCCON_PRSCEN != 0 {
            (((self.adccon >> 6) & 0xFF) as i64 + 1) * 5
        } else {
            5
        };
        let one = self.adcdly as i64 + conv;
        if self.adctsc & ADCTSC_AUTO_PST != 0 {
            2 * one // X と Y を順に測る
        } else {
            one
        }
    }

    fn start(&mut self) {
        self.ecflg = false;
        self.converting = self.conversion_ticks();
    }

    /// 変換完了まであと何ティックか（変換中でなければ NO_EVENT）。
    pub fn next_event(&self) -> i64 {
        if self.converting == 0 {
            NO_EVENT
        } else {
            self.converting.max(1)
        }
    }

    /// 仮想時間を PCLK ティック数だけ進める（変換の完了判定）。
    /// 戻り値は上げたサブソースのビットの束。
    pub fn advance(&mut self, ticks: i64) -> u32 {
        if self.converting == 0 {
            return 0;
        }
        self.converting -= ticks;
        if self.converting > 0 {
            return 0;
        }
        self.converting = 0;
        self.ecflg = true;
        if self.adctsc & ADCTSC_AUTO_PST != 0 {
            (self.dat0, self.dat1) = (self.raw_x, self.raw_y);
        } else if self.xy_pst() == XY_PST_X {
            self.dat0 = self.raw_x;
        } else if self.xy_pst() == XY_PST_Y {
            self.dat1 = self.raw_y;
        } else {
            // 通常の A/D 変換（SEL_MUX のチャネル）。アナログ入力は未接続として 0。
            // TODO: バッテリー電圧等を AIN で測るドライバが現れたら値を与える。
            self.dat0 = 0;
        }
        1 << SUB_ADC
    }

    /// ADCDAT0/1 の上位ビット（UPDOWN・AUTO_PST・XY_PST）。
    fn dat_status(&self) -> u32 {
        let mut v = (self.adctsc & ADCTSC_AUTO_PST) << 12; // bit2 → bit14
        v |= self.xy_pst() << 12;
        if !self.pen_down {
            v |= DAT_UPDOWN;
        }
        v
    }

    /// 副作用のない読み出し（READ_START による変換開始を除いた Read）。
    fn peek(&self, off: u32) -> u32 {
        match off & !3 {
            REG_ADCCON => {
                let v = self.adccon & !(ADCCON_ECFLG | ADCCON_ENABLESTART);
                if self.ecflg { v | ADCCON_ECFLG } else { v }
            }
            REG_ADCTSC => self.adctsc,
            REG_ADCDLY => self.adcdly,
            REG_ADCDAT0 => self.dat_status() | self.dat0,
            REG_ADCDAT1 => self.dat_status() | self.dat1,
            _ => 0,
        }
    }

    pub fn read(&mut self, off: u32, _size: u32) -> u32 {
        let v = self.peek(off);
        if off & !3 == REG_ADCDAT0 && self.adccon & ADCCON_READSTART != 0 && self.converting == 0 {
            self.start(); // READ_START: 読み出しで次の変換を開始
        }
        v
    }

    /// 副作用のない読み出し（bus::Devices::stable_read）。touch.dll は変換完了を ADCCON の
    /// ECFLG のポーリングで待つ（2026-09 観察: touch.dll の 0x015317F4 の
    /// LDR/TST #0x8000/BEQ。操作中の実処理の約 1 割）。ECFLG は変換完了
    /// （next_event の期限）でしか変わらないので、アイドルスキップの対象にできる。
    /// 読み出しに副作用があるのは READ_START 有効時の ADCDAT0（次の変換を開始）
    /// だけなので、それ以外は read と同じ値を返す。
    pub fn stable_read(&self, off: u32, _size: u32) -> Option<u32> {
        if off & !3 == REG_ADCDAT0 && self.adccon & ADCCON_READSTART != 0 {
            return None;
        }
        Some(self.peek(off))
    }

    /// 戻り値は上げたサブソースのビットの束。
    pub fn write(&mut self, off: u32, _size: u32, v: u32) -> u32 {
        match off & !3 {
            REG_ADCCON => {
                self.adccon = v & !ADCCON_ECFLG;
                if v & ADCCON_ENABLESTART != 0 {
                    self.start(); // ENABLE_START は開始後に自動で 0 に戻る（read 参照）
                }
            }
            REG_ADCTSC => {
                let was = self.tc_condition();
                self.adctsc = v & 0x1FF;
                if !was && self.tc_condition() {
                    return 1 << SUB_TC;
                }
            }
            REG_ADCDLY => self.adcdly = v & 0xFFFF,
            _ => {}
        }
        0
    }

    /// ペンを上げる（生値は最後の値のまま）。
    pub fn set_pen_up(&mut self) -> u32 {
        self.set_pen(false, self.raw_x, self.raw_y)
    }
}
