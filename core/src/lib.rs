//! CErulean のエミュレータのコア（Windows Mobile 5.0 / S3C2410 の LLE）。
//!
//! インタプリタ・wasm・JIT でゲストから見える動作を一致させる。
//!
//! コアはプラットフォーム非依存: ファイル・時計・スレッドに触れない。入出力は
//! 呼び出し側（cli・web）から渡す。wasm32 でも同じ結果になるよう、ゲストの値と
//! カウンタは明示の幅（u32/u64）で持ち、`usize` は添字にだけ使う。

pub mod arm;
pub mod bus;
pub mod cpu;
pub mod emu;
pub mod jit;
pub mod loader;
pub mod mmu;
pub mod norflash;
pub mod pccard;
pub mod s3c2410;
pub mod script;
pub mod smdk2410;
pub mod snapshot;

/// 仮想時間 1 秒あたりの命令数。
/// PCLK 50.7MHz、1 命令 = 3/8 PCLK から 50_700_000 × 8 / 3。
pub const INSTRUCTIONS_PER_SECOND: u64 = 135_200_000;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instructions_per_second_matches_pclk() {
        // wasm32（usize が 32 ビット）でも u64 の計算が同じになることの確認を兼ねる。
        assert_eq!(50_700_000u64 * 8 / 3, INSTRUCTIONS_PER_SECOND);
    }
}
