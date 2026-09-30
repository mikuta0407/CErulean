//! DMA コントローラ（データシート Ch.8）と、IIS の送信（I2SSDO）との組み合わせ。

use super::NO_EVENT;
use super::iis::Iis;

/// DMA コントローラ（4 チャネル×0x40 間隔）。
///
/// 各チャネルのレジスタ: DISRC 0x00 / DISRCC 0x04 / DIDST 0x08 / DIDSTC 0x0C /
/// DCON 0x10 / DSTAT 0x14 / DCSRC 0x18 / DCDST 0x1C / DMASKTRIG 0x20。
///
/// 実装しているのは、チャネル 2 の H/W 要求で要求元が I2SSDO（IIS の送信 FIFO）の
/// 転送だけ（音声ドライバ s3c2410x_wavedev.dll が使う形。2026-09-30 に --watch で
/// 確認: DISRC2=RAM のバッファ・DIDST2=IISFIFO・DIDSTC2=3（APB・固定）・
/// DCON2=0xA0900400（ハンドシェイク・割り込みあり・単位転送・シングルサービス・
/// I2SSDO・H/W 要求・自動リロード・ハーフワード・TC=0x400））。
///
/// モデル（データシート 8-2 の DMA の動作と 8-13 の DMASKTRIG・8-14 の注意）:
///   - 要求があると、CURR_TC が 0 なら DCON の TC・DISRC・DIDST を読み込んで
///     （自動リロード。「カウンタが 0 になった後の要求で」行われる）、1 回の原子転送を
///     行い CURR_TC を 1 減らす。0 になったら DCON の INT で割り込みを上げ、RELOAD=1
///     なら ON_OFF を落とす。
///   - 転送は瞬時に終わるものとする（IIS の FIFO が空きを出すたびに埋まる）。
///   - 転送した中身（RAM のサンプル）は、区切り（CURR_TC が 0 になったとき・止めた
///     とき）にまとめて読む（`ready` に積み、ボードが RAM を渡して取り出す）。
///     中身はゲストから見えない（出力の音にだけなる）ので、読む時機は区切りの命令境界に
///     固定して決定論を保つ（転送の瞬間ごとに読むと 1 ハーフワードごとに期限が要る）。
///     バッファを再生中に書き換えるドライバだと実機と音が変わり得る。
///
/// 旧版（2026-09 の最小スタブ）は「ON_OFF を書いたら即完了の割り込み」で、以後の
/// 割り込みを出さなかったため、起動音の再生でドライバが止まり以後の音が鳴らなかった。
///
/// TODO: 未実装・未確認:
///   - I2SSDO 以外の要求元・S/W 要求（SW_TRIG）・メモリ間の転送（今の構成では
///     使われない。要求が来ないものとしてチャネルは進まない）
///   - ホールサービスモード（シングルと同じに扱う）
///   - バイト・ワードの転送を IIS の FIFO（16 ビット幅）に入れたときの扱い（1 単位を
///     FIFO の 1 項目として数え、音としては取り出さない）
///   - DSTAT の STAT（ここでは ON_OFF かつ CURR_TC≠0 の間を busy とする）
///   - DIDST が IISFIFO 以外のとき（宛先は FIFO とみなす）
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Dma {
    pub(crate) ch: [DmaChannel; 4],
    /// 区切りを迎え、まだ RAM から読んでいない転送（派生情報: 保存の前に取り出す）
    pub(crate) ready: Vec<Segment>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DmaChannel {
    pub(crate) disrc: u32,
    pub(crate) disrcc: u32,
    pub(crate) didst: u32,
    pub(crate) didstc: u32,
    pub(crate) dcon: u32,
    /// DMASKTRIG の ON_OFF
    pub(crate) on: bool,
    pub(crate) curr_tc: u32,
    pub(crate) curr_src: u32,
    pub(crate) curr_dst: u32,
    /// 区切りの後に転送した分の始まりの転送元と単位数（音として取り出す範囲）
    pub(crate) seg_src: u32,
    pub(crate) seg_units: u32,
}

/// RAM から読んで音にする転送のまとまり。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Segment {
    /// 転送元の物理アドレス
    pub src: u32,
    /// 転送した単位数
    pub units: u32,
    /// 1 単位のバイト数（1/2/4）
    pub size: u32,
    /// 転送元のアドレスを進めるか（DISRCC の INC=0）
    pub inc: bool,
    /// IIS の 1 フレーム（左右 1 組）の PCLK ティック数
    pub frame_ticks: u32,
}

const REG_DISRC: u32 = 0x00;
const REG_DISRCC: u32 = 0x04;
const REG_DIDST: u32 = 0x08;
const REG_DIDSTC: u32 = 0x0C;
const REG_DCON: u32 = 0x10;
const REG_DSTAT: u32 = 0x14;
const REG_DCSRC: u32 = 0x18;
const REG_DCDST: u32 = 0x1C;
const REG_DMASKTRIG: u32 = 0x20;

const DCON_INT: u32 = 1 << 29;
const DCON_TSZ: u32 = 1 << 28;
const DCON_SWHW: u32 = 1 << 23;
const DCON_RELOAD: u32 = 1 << 22;
const DCON_TC: u32 = 0xFFFFF;
const MASKTRIG_STOP: u32 = 1 << 2;
const MASKTRIG_ON: u32 = 1 << 1;

/// I2SSDO を要求元にできるチャネル（Table 8-1: Ch-2 の Source0）。
const IIS_TX_CH: usize = 2;

impl DmaChannel {
    /// 1 単位のバイト数（DSZ）。11（予約）は TODO: ワードとして扱う。
    fn unit_size(&self) -> u32 {
        match (self.dcon >> 20) & 3 {
            0 => 1,
            1 => 2,
            _ => 4,
        }
    }

    /// 1 回の原子転送の単位数（TSZ: 単位 1 か 4 のバースト）。
    fn atomic_units(&self) -> u32 {
        if self.dcon & DCON_TSZ != 0 { 4 } else { 1 }
    }

    /// I2SSDO の H/W 要求で動くチャネルか（DCON の設定だけを見る）。
    fn iis_tx_source(&self) -> bool {
        self.dcon & DCON_SWHW != 0 && (self.dcon >> 24) & 7 == 0
    }
}

impl Dma {
    pub fn new() -> Dma {
        Dma::default()
    }

    pub fn read(&self, off: u32, _size: u32) -> u32 {
        let Some(c) = self.ch.get((off >> 6) as usize) else {
            return 0;
        };
        match off & 0x3C {
            REG_DISRC => c.disrc,
            REG_DISRCC => c.disrcc,
            REG_DIDST => c.didst,
            REG_DIDSTC => c.didstc,
            REG_DCON => c.dcon,
            REG_DSTAT => {
                let busy = c.on && c.curr_tc != 0;
                (busy as u32) << 20 | c.curr_tc
            }
            REG_DCSRC => c.curr_src,
            REG_DCDST => c.curr_dst,
            // STOP は読み返さない（止めたら ON_OFF が落ちる）。SW_TRIG は要求の
            // 開始で落ちるが、S/W 要求は未実装なので書いた値を残さない。
            REG_DMASKTRIG => (c.on as u32) << 1,
            _ => 0,
        }
    }

    /// レジスタへの書き込み。転送の要求は呼び出し側が続けて
    /// [`service_iis_tx`](Self::service_iis_tx) で処理する。
    pub fn write(&mut self, off: u32, size: u32, v: u32, frame_ticks: u32) {
        let n = (off >> 6) as usize;
        if n >= self.ch.len() {
            return;
        }
        // TODO: 32 ビット以外の書き込み（ドライバは 32 ビットだけを使う）は
        // 下位を書き換えるだけにする。
        let merge = |old: u32| match size {
            1 => old & !0xFF | v & 0xFF,
            2 => old & !0xFFFF | v & 0xFFFF,
            _ => v,
        };
        let c = &mut self.ch[n];
        match off & 0x3C {
            REG_DISRC => c.disrc = merge(c.disrc) & 0x7FFF_FFFF,
            REG_DISRCC => {
                self.finish_segment(n, frame_ticks);
                self.ch[n].disrcc = merge(self.ch[n].disrcc) & 3;
            }
            REG_DIDST => c.didst = merge(c.didst) & 0x7FFF_FFFF,
            REG_DIDSTC => c.didstc = merge(c.didstc) & 3,
            REG_DCON => {
                // DSZ・TSZ の変更は即時に効くので、ここまでの転送を区切る。
                self.finish_segment(n, frame_ticks);
                self.ch[n].dcon = merge(self.ch[n].dcon);
            }
            REG_DMASKTRIG => {
                let m = merge((c.on as u32) << 1);
                if m & MASKTRIG_STOP != 0 {
                    // 原子転送は瞬時なので直ちに止まる。CURR_TC は 0 になる（8-13）。
                    c.on = false;
                    c.curr_tc = 0;
                } else {
                    c.on = m & MASKTRIG_ON != 0;
                }
                if !self.ch[n].on {
                    self.finish_segment(n, frame_ticks);
                }
            }
            _ => {} // DSTAT・DCSRC・DCDST は読み出し専用
        }
    }

    /// チャネル n のまだ読んでいない転送を区切り、`ready` に積む。
    fn finish_segment(&mut self, n: usize, frame_ticks: u32) {
        let c = &mut self.ch[n];
        if c.seg_units == 0 {
            return;
        }
        self.ready.push(Segment {
            src: c.seg_src,
            units: c.seg_units,
            size: c.unit_size(),
            inc: c.disrcc & 1 == 0,
            frame_ticks,
        });
        c.seg_units = 0;
    }

    /// I2SSDO のチャネルが FIFO を埋め続けられる状態か（ON_OFF・要求元・IIS 側の
    /// DMA の要求の許可を見る。FIFO の空きは見ない）。
    fn iis_tx_feeding(&self, iis: &Iis) -> bool {
        let c = &self.ch[IIS_TX_CH];
        c.on && c.iis_tx_source() && iis.tx_dma_enabled()
    }

    /// IIS の送信 FIFO からの DMA の要求を処理する（空きがある間、原子転送を続ける）。
    /// 戻り値は完了割り込みを上げるチャネルのビットの束（bit n = INT_DMAn）。
    #[inline(never)]
    pub fn service_iis_tx(&mut self, iis: &mut Iis) -> u32 {
        let mut ints = 0;
        let frame_ticks = iis.frame_ticks();
        loop {
            if !self.iis_tx_feeding(iis) {
                break;
            }
            let c = &mut self.ch[IIS_TX_CH];
            let units = c.atomic_units();
            if !iis.tx_space(units) {
                break;
            }
            if c.curr_tc == 0 {
                // 自動リロード（初回の読み込みも同じ）
                let tc = c.dcon & DCON_TC;
                if tc == 0 {
                    break; // TODO: TC=0 の動作は不明。転送しない。
                }
                c.curr_tc = tc;
                c.curr_src = c.disrc;
                c.curr_dst = c.didst;
            }
            if c.seg_units == 0 {
                c.seg_src = c.curr_src;
            }
            c.seg_units += units;
            iis.tx_push(units);
            let bytes = c.unit_size() * units;
            if c.disrcc & 1 == 0 {
                c.curr_src = c.curr_src.wrapping_add(bytes);
            }
            if c.didstc & 1 == 0 {
                c.curr_dst = c.curr_dst.wrapping_add(bytes);
            }
            c.curr_tc -= 1;
            if c.curr_tc == 0 {
                let dcon = c.dcon;
                if dcon & DCON_RELOAD != 0 {
                    c.on = false;
                }
                self.finish_segment(IIS_TX_CH, frame_ticks);
                if dcon & DCON_INT != 0 {
                    ints |= 1 << IIS_TX_CH;
                }
            }
        }
        ints
    }

    /// あと何ティック advance すると CURR_TC が 0 になるか（予定がなければ NO_EVENT）。
    /// 割り込みを出さない設定（INT=0）でも期限にする: 区切りの転送を RAM から読む時機を
    /// その命令境界に固定するため（ボードの flush_audio）。FIFO は要求が続く限り満杯に
    /// 保たれるので、IIS が 1 項目送り出すごとに 1 単位ずつ転送が進む。
    #[inline(never)]
    pub fn next_event(&self, iis: &Iis) -> i64 {
        if !iis.tx_running() || !self.iis_tx_feeding(iis) {
            return NO_EVENT;
        }
        let c = &self.ch[IIS_TX_CH];
        if c.atomic_units() != 1 || !iis.tx_full() {
            // バーストは送り出しの何回目で転送するかが FIFO の量に依るので、
            // 次の送り出しを期限にする（早めの期限は結果を変えない）。
            return iis.next_shift();
        }
        let k = if c.curr_tc > 0 {
            c.curr_tc
        } else {
            match c.dcon & DCON_TC {
                0 => return NO_EVENT,
                tc => tc,
            }
        };
        iis.next_shift() + (k as i64 - 1) * iis.half_ticks()
    }

    /// 仮想時間を PCLK ティック数だけ進める（IIS の送り出しと、それが出す DMA の要求）。
    /// 戻り値は完了割り込みのビットの束。
    #[inline(never)]
    pub fn advance(&mut self, iis: &mut Iis, ticks: i64) -> u32 {
        let mut ints = 0;
        let mut t = ticks;
        while let Some(rest) = iis.advance_to_shift(t) {
            t = rest;
            ints |= self.service_iis_tx(iis);
            if iis.tx_empty() && !self.iis_tx_feeding(iis) {
                // 何も送らない送り出しが続くだけなので、まとめて進める。
                iis.idle_shifts(t);
                break;
            }
        }
        ints
    }
}
