//! Samsung S3C2410 SoC の周辺機器。
//! レジスタ仕様の根拠は S3C2410X User's Manual Rev 1.1。
//!
//! S3C2410 固有の知識（レジスタ配置・割り込み番号）はこのモジュールと
//! ボード（smdk2410）だけが持つ。
//!
//! デバイスは「上げた割り込み」を戻り値で返し、ボードがそれを INTC に渡す。
//! INTC は割り込み線のレベルを自分のフィールドに持ち、CPU が命令境界で読む。
//! 割り込みの更新は同じ MMIO アクセスの中で済む。

mod adc;
mod dma;
pub mod eint;
mod iis;
mod intc;
mod lcd;
mod rtc;
mod spi;
mod state;
mod stub;
mod timer;
mod uart;

#[cfg(test)]
mod tests;

pub use adc::Adc;
pub use dma::{Dma, Segment};
pub use iis::Iis;
pub use intc::*;
pub use lcd::{Frame, FrameError, Lcd, LcdConfig};
pub use rtc::Rtc;
pub use spi::{Spi, SpiSlaves};
pub use stub::Stub;
pub use timer::PwmTimer;
pub use uart::Uart;

/// NextEvent の「予定なし」。
pub const NO_EVENT: i64 = i64::MAX;
