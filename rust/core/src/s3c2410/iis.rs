//! IIS バスインターフェース（データシート Ch.21）の送信側。

/// S3C2410 の IIS。送信 FIFO（16 ビット×32 項目）と、それを一定の間隔で送り出す
/// シフトの時間だけを持つ（送り出した音は DMA の転送の側で取り出す。dma.rs）。
///
/// レジスタ（オフセット。リトルエンディアン）:
///
/// ```text
/// 0x00 IISCON  LRI[8](RO) TXFR[7](RO,1=空でない) RXFR[6](RO) TXDMA[5] RXDMA[4]
///              TXIDLE[3] RXIDLE[2] PSEN[1] EN[0]
/// 0x04 IISMOD  SLAVE[8] TXRX[7:6] LRP[5] FMT[4] BITS16[3] MCLK384[2] SCLK[1:0]
/// 0x08 IISPSR  A[9:5] B[4:0]（分周比 N+1）
/// 0x0C IISFCON TXDMA[15] RXDMA[14] TXEN[13] RXEN[12] TXCNT[11:6](RO) RXCNT[5:0](RO)
/// 0x10 IISFIFO
/// ```
///
/// 送り出しの間隔（Table 21-1・IISPSR の説明）: 内部のマスタークロックは
/// PCLK/(A+1) で、これが 256fs か 384fs（IISMOD[2]）。1 フレーム（左右）に FIFO の
/// 2 項目を送るので、1 項目は (A+1)×(256 か 384)/2 PCLK ティック。ドライバの設定
/// （IISPSR=0x42・IISMOD=0xAD。2026-09-30 に --watch で確認）では 3×384/2=576 で、
/// PCLK 50.7MHz から fs≒44.0kHz になる。
///
/// FIFO の DMA の要求（21-3「DMA service request ... is made by the FIFO ready flag
/// automatically」）: 送信 FIFO が有効（TXEN）・DMA モード（IISFCON の TXDMA）・
/// 要求が許可（IISCON の TXDMA）のとき、空きがあれば要求する。
/// IIS の EN が 0 でも要求するとした（ドライバは DMA を ON にしてから FIFO が
/// 空でないこと（TXFR）を待って EN を立てる。2026-09-30 の観察）。
///
/// 送信 FIFO を無効にする（IISFCON の TXEN を 1→0）と中身を捨てる（判断）。データシート
/// には書かれていないが、ドライバは再生を止めるときに DMA を STOP → TXIDLE → TXEN=0 →
/// 送信なし（IISMOD）とし、次の再生では DMA を ON にしてから CURR_TC≠0（DMA の要求で
/// 読み込まれた）を待つ（8-14 の S/W Work-Around）。FIFO に残っていると要求が出ずに
/// 待ちが終わらないので、実機ではこの手順のどこかで空になるはず（2026-09-30 の観察）。
///
/// TODO: 未実装・未確認:
///   - FIFO が空になる本当の条件（TXEN=0 以外に EN=0・送信なしでも空になるか）
///   - 受信（RX FIFO は常に空。RXFR は 0 を返す）
///   - スレーブモード（外部のクロック。マスターと同じ間隔とする）・8 ビットの
///     データ（FIFO の 1 項目を 1 チャネルとして同じ間隔とする）
///   - プリスケーラ無効（PSEN=0）時のマスタークロック（PCLK そのものとする）
///   - 送り出しの途中で分周を変えたときの次の送り出しの時刻（残りをそのまま使う）
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Iis {
    /// IISCON の書ける部分（[5:0]）
    pub(crate) con: u32,
    pub(crate) mode: u32,
    pub(crate) psr: u32,
    /// IISFCON の書ける部分（[15:12]）
    pub(crate) fcon: u32,
    /// 送信 FIFO の項目数（0〜32）
    pub(crate) tx_count: u32,
    /// 次の送り出しまでの PCLK ティック（送り出し中は 1 以上）
    pub(crate) phase: i64,
    /// 左右の表示（IISCON[8]。1 = 右）
    pub(crate) right: bool,
}

pub(crate) const REG_IISCON: u32 = 0x00;
pub(crate) const REG_IISMOD: u32 = 0x04;
pub(crate) const REG_IISPSR: u32 = 0x08;
pub(crate) const REG_IISFCON: u32 = 0x0C;
pub(crate) const REG_IISFIFO: u32 = 0x10;

const CON_EN: u32 = 1 << 0;
const CON_PSEN: u32 = 1 << 1;
const CON_TXIDLE: u32 = 1 << 3;
const CON_TXDMA: u32 = 1 << 5;
const MOD_TX: u32 = 1 << 7;
const MOD_MCLK384: u32 = 1 << 2;
const FCON_TXDMA: u32 = 1 << 15;
const FCON_TXEN: u32 = 1 << 13;

/// 送信 FIFO の深さ（16 ビット×32。21-8）。
const FIFO_DEPTH: u32 = 32;

impl Default for Iis {
    fn default() -> Self {
        Self::new()
    }
}

impl Iis {
    /// リセット値: IISCON=0x100（LRI=1）、他は 0（21-5〜21-8）。
    pub fn new() -> Iis {
        Iis {
            con: 0,
            mode: 0,
            psr: 0,
            fcon: 0,
            tx_count: 0,
            phase: 0,
            right: true,
        }
    }

    /// FIFO の 1 項目を送り出す間隔（PCLK ティック）。
    pub fn half_ticks(&self) -> i64 {
        let div = if self.con & CON_PSEN != 0 {
            ((self.psr >> 5) & 0x1F) as i64 + 1
        } else {
            1
        };
        let fs = if self.mode & MOD_MCLK384 != 0 {
            384
        } else {
            256
        };
        div * fs / 2
    }

    /// 1 フレーム（左右 1 組）の PCLK ティック数。
    pub fn frame_ticks(&self) -> u32 {
        (self.half_ticks() * 2) as u32
    }

    /// 送信のシフトが動いているか。
    pub fn tx_running(&self) -> bool {
        self.con & CON_EN != 0 && self.mode & MOD_TX != 0 && self.con & CON_TXIDLE == 0
    }

    /// 送信 FIFO が DMA の要求を出せる設定か（空きは見ない）。
    pub fn tx_dma_enabled(&self) -> bool {
        self.fcon & FCON_TXDMA != 0 && self.fcon & FCON_TXEN != 0 && self.con & CON_TXDMA != 0
    }

    /// n 項目の空きがあるか。
    pub fn tx_space(&self, n: u32) -> bool {
        self.tx_count + n <= FIFO_DEPTH
    }

    pub fn tx_full(&self) -> bool {
        self.tx_count == FIFO_DEPTH
    }

    pub fn tx_empty(&self) -> bool {
        self.tx_count == 0
    }

    /// DMA が FIFO に n 項目入れる（空きは呼び出し側が確かめる）。
    pub fn tx_push(&mut self, n: u32) {
        self.tx_count += n;
    }

    /// 次の送り出しまでのティック数（送り出し中のとき）。
    pub fn next_shift(&self) -> i64 {
        self.phase
    }

    /// t ティックの間に送り出しが起きるなら 1 項目送り出し、残りのティックを返す。
    /// 起きないなら t だけ進めて None（止まっていれば何もしない）。
    pub fn advance_to_shift(&mut self, t: i64) -> Option<i64> {
        if !self.tx_running() {
            return None;
        }
        if t < self.phase {
            self.phase -= t;
            return None;
        }
        let rest = t - self.phase;
        self.phase = self.half_ticks();
        self.tx_count = self.tx_count.saturating_sub(1); // 空なら送るものがない
        self.right = !self.right;
        Some(rest)
    }

    /// FIFO が空で補充もない間: t ティック分の送り出しをまとめて進める。
    pub fn idle_shifts(&mut self, t: i64) {
        if !self.tx_running() {
            return;
        }
        if t < self.phase {
            self.phase -= t;
            return;
        }
        let p = self.half_ticks();
        let rest = t - self.phase;
        let n = 1 + rest / p;
        self.right ^= n & 1 != 0;
        self.phase = p - rest % p;
    }

    pub fn read(&self, off: u32, _size: u32) -> u32 {
        match off & !3 {
            REG_IISCON => (self.right as u32) << 8 | ((self.tx_count != 0) as u32) << 7 | self.con,
            REG_IISMOD => self.mode,
            REG_IISPSR => self.psr,
            REG_IISFCON => self.fcon | self.tx_count << 6,
            _ => 0, // IISFIFO の読み出しは受信（未実装）
        }
    }

    /// 書き込み。IISFIFO に CPU が書いた値は、FIFO に入れば Some で返す（音にする）。
    pub fn write(&mut self, off: u32, size: u32, v: u32) -> Option<u16> {
        let was = self.tx_running();
        let merge = |old: u32| match size {
            1 => old & !0xFF | v & 0xFF,
            _ => v, // ハーフワード・ワード（レジスタは 16 ビット以内）
        };
        let mut sample = None;
        match off & !3 {
            REG_IISCON => self.con = merge(self.con) & 0x3F,
            REG_IISMOD => self.mode = merge(self.mode) & 0x1FF,
            REG_IISPSR => self.psr = merge(self.psr) & 0x3FF,
            REG_IISFCON => {
                let old = self.fcon;
                self.fcon = merge(self.fcon) & 0xF000;
                if old & FCON_TXEN != 0 && self.fcon & FCON_TXEN == 0 {
                    self.tx_count = 0; // 送信 FIFO を無効にしたら捨てる（先頭のコメント）
                }
            }
            // TODO: TXEN=0 のときの書き込みは捨てる（未確認）
            REG_IISFIFO if self.fcon & FCON_TXEN != 0 && self.tx_space(1) => {
                self.tx_count += 1;
                sample = Some(v as u16);
            }
            _ => {}
        }
        let now = self.tx_running();
        if now && !was {
            self.phase = self.half_ticks();
        } else if now {
            self.phase = self.phase.min(self.half_ticks());
        }
        sample
    }
}
