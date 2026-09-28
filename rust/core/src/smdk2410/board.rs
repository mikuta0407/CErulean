//! ボードのデバイス群・MMIO の振り分け・仮想時間（Go の smdk2410.go の構成部分と
//! run.go の時間同期・timedDev）。

use crate::arm::RunCtl;
use crate::bus::Devices;
use crate::s3c2410::*;

use super::kbd::KbdMcu;
use super::{PCLK_HZ, PCLK_TICKS_NUM};

/// バスに登録する MMIO デバイスの識別子（Go では bus.Device の値そのものを
/// 登録していた。Rust ではデバイスをボードが持ち、バスには番号だけを置く）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dev {
    /// SDRAM が実装されていないバンク窓（読み 0・書き無視）
    OpenBus,
    Uart(u8),
    Intc,
    Timer,
    Lcd,
    Rtc,
    Adc,
    Spi,
    Dma,
    Stub(StubId),
}

/// 値保持スタブのブロック（S3C2410 データシート Figure 5-1）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StubId {
    /// メモリコントローラ（BWSCON など）
    Memc = 0,
    UsbHost,
    /// クロック・電源管理
    ClkPwr,
    /// NAND フラッシュコントローラ
    Nand,
    Wdt,
    Iic,
    UsbDev,
    Sdi,
    Gpio,
    /// IIS（オーディオ）
    Iis,
    /// 0x500F0000: Device Emulator 固有の準仮想デバイス群（smdk2410 の構成のコメント参照）
    DeParavirt,
}

pub const NUM_STUBS: usize = 11;

/// ボード上のデバイス群と仮想時間（Go の Machine のデバイスと時間のフィールド）。
/// バスとは別のフィールドなので、バスの読み書きに `&mut Board` を渡せる。
pub struct Board {
    pub intc: Intc,
    pub timer: PwmTimer,
    pub lcd: Lcd,
    pub rtc: Rtc,
    pub adc: Adc,
    pub spi: Spi,
    pub kbd: KbdMcu,
    pub uart: [Uart; 3],
    pub dma: DmaStub,
    pub stubs: [Stub; NUM_STUBS],

    /// 仮想時間: 命令数から PCLK ティックを固定比で生成する（決定論的。
    /// ユーザー確認済み 2026-09）。tick_acc は 1/8 ティック単位の端数累積（保存する）。
    pub(crate) tick_acc: u32,
    /// リセットからの実行命令数（= 仮想時間の基準。保存する）。
    pub(crate) steps: u64,

    // 実行ループの作業領域（run.rs。いずれも保存しない）:
    /// デバイスにまだ渡していない PCLK ティック
    pub(crate) pending: i64,
    /// 次のデバイスイベントまでのティック数（NO_EVENT なら予定なし）
    pub(crate) deadline: i64,
    /// CPU のブロック実行中（catch_up）
    pub(crate) in_run: bool,
    /// ブロック実行中、steps に反映済みの命令数
    pub(crate) accounted: u64,
    /// CPU の実行の上限（Go の runN/runBudget）
    pub(crate) run: RunCtl,
}

impl Board {
    pub(crate) fn new() -> Board {
        let stub = |init: &[(u32, u32)]| Stub::new(init);
        Board {
            intc: Intc::new(),
            timer: PwmTimer::new(),
            lcd: Lcd::new(),
            rtc: Rtc::new(PCLK_HZ),
            adc: Adc::new(),
            spi: Spi::new(),
            kbd: KbdMcu::default(),
            // UART1 がカーネルデバッグシリアル（2026-09 に実測。ブートバナーが
            // UART1 の UTXH に書かれた）。TODO: UART0/2 の出力先はアプリの
            // シリアル対応時に決める。
            uart: [Uart::new(false), Uart::new(true), Uart::new(false)],
            dma: DmaStub::new(),
            stubs: [
                stub(&[]), // memc
                stub(&[]), // usbhost
                // クロック・電源管理のリセット値（User's Manual Rev 1.1 の Ch.7 で
                // 確認済み）。カーネルが PLL 設定からクロックを逆算する場合に 0 だと
                // 壊れるため入れておく。
                stub(&[
                    (0x00, 0x00FFFFFF), // LOCKTIME
                    (0x04, 0x0005C080), // MPLLCON
                    (0x08, 0x00028080), // UPLLCON
                    (0x0C, 0x0007FFF0), // CLKCON
                    (0x10, 0x00000004), // CLKSLOW
                ]),
                stub(&[]),               // nand
                stub(&[(0x00, 0x8021)]), // WTCON リセット値（User's Manual Rev 1.1 で確認済み）
                stub(&[]),               // iic
                stub(&[]),               // usbdev
                stub(&[]),               // sdi
                // GSTATUS1: チップ ID。BSP が SoC 判別に読む可能性がある。
                // 0x32410000（User's Manual Rev 1.1 で確認済み）
                stub(&[(0xB0, 0x32410000)]),
                // IIS（オーディオ）: 値保持スタブだが、IISCON(0x00) の bit7
                // （TX FIFO ready）は常に立てる。FIFO は無限シンク扱いで、オーディオ
                // ドライバの送信 ready 待ちポーリングを通すため（2026-09 に実測）。
                // TODO: 音を出すときは FIFO・DMA 込みの実装に置き換える。
                stub(&[]).force_read_bits(0x00, 1 << 7),
                stub(&[]), // de-paravirt
            ],
            tick_acc: 0,
            steps: 0,
            pending: 0,
            deadline: NO_EVENT,
            in_run: false,
            accounted: 0,
            run: RunCtl::default(),
        }
    }

    // ---- 仮想時間（Go の run.go）----
    //
    // デバイス時間のまとめ進め（性能対策。ユーザー確認済み 2026-09）: 仮想時間の
    // PCLK ティックは毎命令ではなく pending に溜め、次の 2 つの時点でだけデバイスの
    // advance を呼ぶ。
    //   - 溜まったティックが「次のデバイスイベント（タイマー満了・ADC 変換
    //     完了）」の期限に達した命令の直後（1 命令ずつ進めた場合と同じ命令境界）
    //   - CPU（またはホスト側の入力 API）が時間を持つデバイスに触れる直前
    //     （Go の timedDev）。レジスタ読み出しが常に最新の時刻を反映する。
    // 期限より手前では advance は加算的（advance(a)+advance(b) == advance(a+b)）
    // なので、全状態が毎命令 advance した場合と一致する。

    /// 今から何命令目で次のデバイスイベントの期限のティックが生まれるか（1 以上）。
    /// (tick_acc + PCLK_TICKS_NUM*k)/8 >= deadline-pending の最小の k。
    /// Go と同じく 64 ビットの折り返しで計算する（期限に達したら即 sync_time する
    /// ので need は 1 以上のはず）。
    pub(crate) fn steps_to_deadline(&self) -> u64 {
        let need = self.deadline.wrapping_sub(self.pending) as u64;
        need.wrapping_mul(8)
            .wrapping_sub(self.tick_acc as u64)
            .wrapping_add(PCLK_TICKS_NUM - 1)
            / PCLK_TICKS_NUM
    }

    /// ブロック実行中（または直後）に、CPU が実行を終えた命令の分だけ
    /// 命令数と仮想時間を進める。
    pub(crate) fn catch_up(&mut self) {
        if !self.in_run {
            return;
        }
        let done = self.run.n;
        let n = done - self.accounted;
        self.accounted = done;
        self.steps += n;
        self.add_ticks(n);
    }

    /// n 命令分のティックを pending に足す（1 命令 = 3/8 PCLK。1/8 ティックの
    /// 端数を持ち越す）。
    pub(crate) fn add_ticks(&mut self, n: u64) {
        let total = self.tick_acc as u64 + PCLK_TICKS_NUM * n;
        self.pending += (total >> 3) as i64;
        self.tick_acc = (total & 7) as u32;
    }

    /// 溜めたティックをデバイスに渡し、次の期限を求め直す。
    pub(crate) fn sync_time(&mut self) {
        self.catch_up();
        if self.pending > 0 {
            let t = self.pending;
            self.pending = 0;
            let fired = self.timer.advance(t);
            self.intc.raise_mask(fired << INT_TIMER0);
            self.rtc.advance(t);
            let subs = self.adc.advance(t);
            self.intc.raise_sub_mask(subs);
        }
        self.update_deadline();
    }

    /// 次のデバイスイベントまでのティック数を求める。時間を持つデバイスの
    /// 状態が変わったら（レジスタ書き込み・入力）呼ぶ。ブロック実行中なら、
    /// 新しい期限を生む命令で止まるよう CPU の上限を下げる（実行中の命令も
    /// 1 命令目に数える: その命令のティックも期限に向けて進むため）。
    pub(crate) fn update_deadline(&mut self) {
        self.deadline = self.timer.next_event().min(self.adc.next_event());
        if self.in_run && self.deadline != NO_EVENT {
            let lim = self.accounted + self.steps_to_deadline();
            self.run.limit(lim);
        }
    }

    /// Steps（Go の Machine.Steps）。ブロック実行の途中（デバイスのアクセス中）でも、
    /// 実行を終えた命令まで数えた値を返す。
    pub fn steps(&self) -> u64 {
        if self.in_run {
            self.steps + self.run.n - self.accounted
        } else {
            self.steps
        }
    }
}

/// SPI1 のキーボード用マイコンと、EINT1 を上げる INTC の組（SPI の転送中に使う）。
struct KbdPort<'a> {
    kbd: &'a mut KbdMcu,
    intc: &'a mut Intc,
}

impl SpiSlaves for KbdPort<'_> {
    fn transfer(&mut self, ch: usize, tx: u8) -> Option<u8> {
        if ch != 1 {
            return None; // SPI0 には何もつながっていない
        }
        let (b, raise) = self.kbd.transfer(tx);
        if raise {
            self.intc.raise(INT_EINT0 + 1); // EINT1
        }
        Some(b)
    }
}

impl Devices<Dev> for Board {
    fn read(&mut self, dev: Dev, off: u32, size: u32) -> u32 {
        match dev {
            Dev::OpenBus => 0,
            Dev::Uart(n) => self.uart[n as usize].read(off, size),
            Dev::Intc => self.intc.read(off, size),
            Dev::Lcd => self.lcd.read(off, size),
            Dev::Spi => self.spi.read(off, size),
            Dev::Dma => self.dma.read(off, size),
            Dev::Stub(s) => self.stubs[s as usize].read(off, size),
            // 時間を持つデバイス（Go の timedDev）: アクセスの前に溜めたティックを
            // 渡し、後で期限を求め直す（読み出しで変換が始まる ADC の READ_START の
            // ように、読み出しも状態を変え得るため）。
            Dev::Timer | Dev::Rtc | Dev::Adc => {
                self.sync_time();
                let v = match dev {
                    Dev::Timer => self.timer.read(off, size),
                    Dev::Rtc => self.rtc.read(off, size),
                    _ => self.adc.read(off, size),
                };
                self.update_deadline();
                v
            }
        }
    }

    fn write(&mut self, dev: Dev, off: u32, size: u32, v: u32) {
        match dev {
            Dev::OpenBus => {}
            Dev::Uart(n) => self.uart[n as usize].write(off, size, v),
            Dev::Intc => self.intc.write(off, size, v),
            Dev::Lcd => self.lcd.write(off, size, v),
            Dev::Spi => {
                let mut port = KbdPort {
                    kbd: &mut self.kbd,
                    intc: &mut self.intc,
                };
                if let Some(ch) = self.spi.write(off, size, v, &mut port) {
                    self.intc.raise([INT_SPI0, INT_SPI1][ch]);
                }
            }
            Dev::Dma => {
                if let Some(ch) = self.dma.write(off, size, v) {
                    self.intc.raise(INT_DMA0 + ch as u32);
                }
            }
            Dev::Stub(s) => self.stubs[s as usize].write(off, size, v),
            Dev::Timer | Dev::Rtc | Dev::Adc => {
                self.sync_time();
                match dev {
                    Dev::Timer => self.timer.write(off, size, v),
                    Dev::Rtc => self.rtc.write(off, size, v),
                    _ => {
                        let subs = self.adc.write(off, size, v);
                        self.intc.raise_sub_mask(subs);
                    }
                }
                self.update_deadline();
            }
        }
    }

    fn stable_read(&mut self, dev: Dev, off: u32, size: u32) -> Option<u32> {
        match dev {
            // 時間を同期してから読むので read と同じ値になる（同期は状態の見え方を
            // 変えない）。Go の timedDev.StableRead は期限を求め直さない。
            Dev::Adc => {
                self.sync_time();
                self.adc.stable_read(off, size)
            }
            _ => None,
        }
    }
}
