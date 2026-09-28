//! CErulean のブラウザ版のエントリ（Web Worker から使う wasm の API）。
//! 段階2・3 で Worker 用の API を足す（docs/rust-migration-plan.md §3.4・§7.2）。

use wasm_bindgen::prelude::*;

/// 仮想時間 1 秒あたりの命令数（骨組みの確認用。JS から呼べることを確かめる）。
/// u64 は JS では BigInt になる。
#[wasm_bindgen(js_name = instructionsPerSecond)]
pub fn instructions_per_second() -> u64 {
    cerulean_core::INSTRUCTIONS_PER_SECOND
}
