//! PC カード（PCMCIA）: コントローラとカード。特定の SoC・ボードに依らない部品で、
//! バスのどこに置き、割り込みをどこへ配線するかはボード（smdk2410）が決める。

pub mod cf;
pub mod ne2000;
pub mod pd6710;

pub use cf::CfCard;
pub use ne2000::Ne2000;
pub use pd6710::{Card, Pd6710, Space};

/// ソケットに挿さるカードの種類。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Slot {
    /// CompactFlash のストレージカード
    Cf(CfCard),
    /// NE2000 互換のイーサネットカード
    Nic(Ne2000),
}

impl Slot {
    pub fn as_card(&mut self) -> &mut dyn Card {
        match self {
            Slot::Cf(c) => c,
            Slot::Nic(n) => n,
        }
    }

    pub fn as_card_ref(&self) -> &dyn Card {
        match self {
            Slot::Cf(c) => c,
            Slot::Nic(n) => n,
        }
    }
}
