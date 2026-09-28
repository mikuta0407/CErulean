//! SPI コントローラ（データシート Ch.22）の 2 チャネル分。

/// SPI バスの相手デバイス（ボードがつなぐ）。transfer はチャネル ch で送信
/// バイトを受け取り、同時に返すバイトを返す（全二重で 1 バイト交換）。
/// 何もつながっていなければ None（0 を受信する）。
pub trait SpiSlaves {
    fn transfer(&mut self, ch: usize, tx: u8) -> Option<u8>;
}

/// チャネル n のレジスタは 0x20*n からの 6 本:
///
/// ```text
/// 0x00 SPCON  SMOD[6:5] ENSCK[4] MSTR[3] CPOL[2] CPHA[1] TAGD[0]
/// 0x04 SPSTA  DCOL[2] MULF[1] REDY[0]（読み出し専用）
/// 0x08 SPPIN  ENMUL[2] KEEP[0]
/// 0x0C SPPRE  プリスケーラ
/// 0x10 SPTDAT 送信データ（書くと転送開始）
/// 0x14 SPRDAT 受信データ
/// ```
///
/// モデル: マスターとして SPTDAT に書いた瞬間に 1 バイトの交換が完了する
/// （転送時間は 0。REDY は常に 1）。SMOD=01（割り込みモード）なら転送
/// 完了ごとに INT_SPIn を上げる（write の戻り値でチャネルを返す）。
///
/// 常に REDY=1 にする理由: ブート時にドライバが SPSTA1 の REDY をポーリング
/// するため（2026-09 に実測。以前は値保持スタブで REDY を立てていた）。
///
/// レジスタ配置・SMOD（00 ポーリング/01 割り込み/10 DMA）・リセット値・
/// INT_SPI0/1（22/29）は User's Manual Rev 1.1 の Ch.22・Ch.14 で確認済み。
/// TODO: DMA モード（SMOD=10）とスレーブモード、TAGD（自動ガベージ送信）、
/// DCOL/MULF は未実装。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Spi {
    pub(crate) ch: [SpiChannel; 2],
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct SpiChannel {
    pub(crate) spcon: u32,
    pub(crate) sppin: u32,
    pub(crate) sppre: u32,
    pub(crate) rx: u32,
}

const REG_SPCON: u32 = 0x00;
const REG_SPSTA: u32 = 0x04;
const REG_SPPIN: u32 = 0x08;
const REG_SPPRE: u32 = 0x0C;
const REG_SPTDAT: u32 = 0x10;
const REG_SPRDAT: u32 = 0x14;
const SPSTA_REDY: u32 = 1 << 0;
const SMOD_INT: u32 = 1; // SPCON[6:5]=01: 割り込みモード

impl Default for Spi {
    fn default() -> Self {
        Self::new()
    }
}

impl Spi {
    pub fn new() -> Spi {
        // リセット値 0x02（KEEP[0]=0、予約 bit1 は「1 にすること」）。
        let c = SpiChannel {
            sppin: 0x02,
            ..Default::default()
        };
        Spi { ch: [c.clone(), c] }
    }

    pub fn read(&self, off: u32, _size: u32) -> u32 {
        let Some(c) = self.ch.get((off / 0x20) as usize) else {
            return 0;
        };
        match (off % 0x20) & !3 {
            REG_SPCON => c.spcon,
            REG_SPSTA => SPSTA_REDY,
            REG_SPPIN => c.sppin,
            REG_SPPRE => c.sppre,
            REG_SPRDAT => c.rx,
            _ => 0,
        }
    }

    /// 戻り値は転送完了の割り込みを上げるチャネル（割り込みモードの SPTDAT 書き込み）。
    pub fn write(
        &mut self,
        off: u32,
        _size: u32,
        v: u32,
        slaves: &mut impl SpiSlaves,
    ) -> Option<usize> {
        let n = (off / 0x20) as usize;
        let c = self.ch.get_mut(n)?;
        match (off % 0x20) & !3 {
            REG_SPCON => c.spcon = v & 0x7F,
            REG_SPPIN => c.sppin = v & 0x7,
            REG_SPPRE => c.sppre = v & 0xFF,
            REG_SPTDAT => {
                c.rx = slaves.transfer(n, v as u8).map_or(0, |b| b as u32);
                if (c.spcon >> 5) & 3 == SMOD_INT {
                    return Some(n);
                }
            }
            _ => {}
        }
        None
    }
}
