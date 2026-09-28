//! Samsung S3C2410 SoC の周辺機器（Go の device/s3c2410 パッケージ）。
//! レジスタ仕様の根拠は S3C2410X User's Manual Rev 1.1。
//!
//! S3C2410 固有の知識（レジスタ配置・割り込み番号）はこのモジュールと
//! ボード（smdk2410）だけが持つ。
//!
//! 所有権の設計（計画書 §3.3 の案 A）: Go ではデバイスがコールバック（raise）で
//! INTC を呼び、INTC がコールバックで CPU の割り込み線を動かしていた。Rust では
//! デバイスは「上げた割り込み」を戻り値で返し、ボードがそれを INTC に渡す。
//! INTC は割り込み線のレベルを自分のフィールドに持ち、CPU が命令境界で読む。
//! 効果の起きる命令境界は Go と同じ（どれも同じ MMIO アクセスの中で済む）。

mod adc;
mod dma;
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
pub use dma::DmaStub;
pub use intc::*;
pub use lcd::{Frame, FrameError, Lcd, LcdConfig};
pub use rtc::Rtc;
pub use spi::{Spi, SpiSlaves};
pub use stub::Stub;
pub use timer::PwmTimer;
pub use uart::Uart;

/// NextEvent の「予定なし」。
pub const NO_EVENT: i64 = i64::MAX;
