//! PC カード（PCMCIA）: コントローラとカード。特定の SoC・ボードに依らない部品で、
//! バスのどこに置き、割り込みをどこへ配線するかはボード（smdk2410）が決める。

pub mod cf;
pub mod pd6710;

pub use cf::CfCard;
pub use pd6710::{Card, Pd6710, Space};
