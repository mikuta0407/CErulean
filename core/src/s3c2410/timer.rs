//! PWM タイマー（データシート Ch.10）。

use super::NO_EVENT;

/// S3C2410 の PWM タイマーのうち、カウントダウンと割り込み生成だけを実装する
/// （TOUT 出力・PWM 波形・デッドゾーンは未実装。WinCE はシステムティックに
/// Timer4 を使う）。
///
/// 時間は machine が命令数ベースの仮想時間で進める（advance に PCLK
/// ティック数を渡す。ユーザー確認済み 2026-09）。各タイマーの残り時間は
/// PCLK ティック単位（カウント値 × スケール）で保持する。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PwmTimer {
    pub(crate) tcfg0: u32,
    pub(crate) tcfg1: u32,
    pub(crate) tcon: u32,
    pub(crate) tcntb: [u32; 5],
    /// Timer4 に TCMPB はない
    pub(crate) tcmpb: [u32; 4],
    /// 残り PCLK ティック（<=0 は停止中/満了）
    pub(crate) cnt: [i64; 5],
    pub(crate) running: [bool; 5],
}

const REG_TCFG0: u32 = 0x00;
const REG_TCFG1: u32 = 0x04;
const REG_TCON: u32 = 0x08;
// 以降 TCNTBn/TCMPBn/TCNTOn（Timer4 は TCNTB4=0x3C, TCNTO4=0x40）

/// TCON 内の Timer n の (start, manual, autoreload) ビット。
/// Timer0: [3:0]、Timer1-3: [11:8]/[15:12]/[19:16]、Timer4: [22:20]
/// （Timer4 はインバータビットがないためリロードが +2）。
fn tcon_bits(n: usize) -> (u32, u32, u32) {
    match n {
        0 => (1 << 0, 1 << 1, 1 << 3),
        4 => (1 << 20, 1 << 21, 1 << 22),
        _ => {
            let shift = 4 + 4 * n as u32; // Timer1=8, 2=12, 3=16
            (1 << shift, 1 << (shift + 1), 1 << (shift + 3))
        }
    }
}

/// 0x0C 以降のレジスタを（タイマー番号, 種別）に変換する。
/// 種別: 0=TCNTB, 1=TCMPB, 2=TCNTO。
fn timer_reg(off: u32) -> Option<(usize, u32)> {
    if !(0x0C..=0x40).contains(&off) {
        return None;
    }
    let i = (off - 0x0C) / 4; // 0..13
    if i < 12 {
        return Some(((i / 3) as usize, i % 3)); // Timer0-3: 3 レジスタずつ
    }
    // Timer4: TCNTB4(0x3C), TCNTO4(0x40)
    Some((4, if i == 12 { 0 } else { 2 }))
}

impl PwmTimer {
    pub fn new() -> PwmTimer {
        PwmTimer::default()
    }

    /// Timer n の 1 カウントあたりの PCLK ティック数（(プリスケーラ+1) × 分周比）。
    fn scale(&self, n: usize) -> i64 {
        let presc = if n >= 2 {
            (self.tcfg0 >> 8) & 0xFF
        } else {
            self.tcfg0 & 0xFF
        };
        let mux = (self.tcfg1 >> (4 * n as u32)) & 0xF;
        // 外部 TCLK / TOUT 入力（mux >= 4）は未対応。
        // TODO: 必要になったら実装する。当面は最大分周(1/16)として扱う。
        let div = if mux >= 4 { 16 } else { 2i64 << (mux & 3) };
        (presc as i64 + 1) * div
    }

    /// 自動リロード 1 周期分の PCLK ティック数 = (TCNTB+1) カウント。
    /// 根拠: User's Manual Rev 1.1 の Figure 10-2（TCNTB=3 で TCNT が
    /// 3→2→1→0 と進み、0 の 1 カウントの後にリロード）と、最大間隔の表
    /// （65535 で 65536 カウント分）。マニュアルアップデート直後の最初の満了は
    /// TCNTB カウント後（TCNT=TCNTB から 0 に達した時点で割り込み）。
    fn period(&self, n: usize) -> i64 {
        (self.tcntb[n] as i64 + 1) * self.scale(n)
    }

    /// 仮想時間を PCLK ティック数だけ進め、満了したタイマーの番号のビット
    /// （bit n = Timer n）を返す（ボードが INTC の INT_TIMERn に渡す）。
    pub fn advance(&mut self, ticks: i64) -> u32 {
        let mut fired = 0;
        for n in 0..5 {
            if !self.running[n] {
                continue;
            }
            self.cnt[n] -= ticks;
            while self.cnt[n] <= 0 {
                fired |= 1 << n;
                let (_, _, reload) = tcon_bits(n);
                if self.tcon & reload == 0 {
                    self.running[n] = false; // ワンショット: 停止
                    self.cnt[n] = 0;
                    break;
                }
                self.cnt[n] += self.period(n); // 自動リロード
            }
        }
        fired
    }

    /// あと何ティック advance すると割り込みが上がる（= カウンタの線形な
    /// 減少以外の状態変化が起きる）かを返す。予定がなければ NO_EVENT。
    /// machine はこれを期限として advance をまとめて呼ぶ（性能対策）。期限より
    /// 手前までは advance(a)+advance(b) と advance(a+b) の結果が一致する。
    pub fn next_event(&self) -> i64 {
        let mut next = NO_EVENT;
        for n in 0..5 {
            if self.running[n] {
                // cnt<=0 で動作中なら、次の 1 ティックで満了する（advance 参照）。
                next = next.min(self.cnt[n].max(1));
            }
        }
        next
    }

    pub fn read(&self, off: u32, _size: u32) -> u32 {
        match off & !3 {
            REG_TCFG0 => return self.tcfg0,
            REG_TCFG1 => return self.tcfg1,
            REG_TCON => return self.tcon,
            _ => {}
        }
        match timer_reg(off & !3) {
            Some((n, 0)) => self.tcntb[n],
            Some((n, 1)) => self.tcmpb[n],
            Some((n, _)) => {
                // TCNTO: 現在値をスケールから逆算
                let s = self.scale(n);
                if s > 0 && self.cnt[n] > 0 {
                    (self.cnt[n] / s) as u32
                } else {
                    0
                }
            }
            None => 0,
        }
    }

    pub fn write(&mut self, off: u32, _size: u32, v: u32) {
        match off & !3 {
            REG_TCFG0 => self.tcfg0 = v,
            REG_TCFG1 => self.tcfg1 = v,
            REG_TCON => {
                let old = self.tcon;
                self.tcon = v;
                for n in 0..5 {
                    let (start, manual, _) = tcon_bits(n);
                    if v & manual != 0 {
                        // マニュアルアップデート: TCNTB を内部カウンタにロード。
                        self.cnt[n] = self.tcntb[n] as i64 * self.scale(n);
                    }
                    if v & start != 0 && old & start == 0 {
                        // スタート。マニュアルアップデートを経ずに開始された場合は
                        // リロード値から数える。
                        if self.cnt[n] <= 0 {
                            self.cnt[n] = self.period(n);
                        }
                        self.running[n] = true;
                    } else if v & start == 0 {
                        self.running[n] = false;
                    }
                }
            }
            o => match timer_reg(o) {
                Some((n, 0)) => self.tcntb[n] = v,
                Some((n, 1)) if n < 4 => self.tcmpb[n] = v,
                _ => {} // TCNTO は読み出し専用
            },
        }
    }
}
