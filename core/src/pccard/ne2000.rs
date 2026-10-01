//! NE2000 互換の PC カード（イーサネット）。コントローラは DP8390 相当。
//!
//! 一次資料:
//!   - National Semiconductor DP83902A ST-NIC データシート（以下「DS」）: レジスタ
//!     （10 章）・受信のバッファリング（7 章）・送信（8 章）・リモート DMA（9 章）・
//!     初期化（11 章）
//!   - ASIX AX88190 データシート（以下「AX」）: NE2000 互換の PC カードの I/O の
//!     並び（Tab-14: 00h〜0Fh が DP8390 のレジスタ、10h がデータポート、1Fh がリセット）
//!   - CIS の書式は cf.rs と同じく SanDisk の Table 6-1 の各フィールドの説明に従う
//!
//! モデル: 送信は TXP を立てた時点で完了する（ホストへの送信の列に積む）。受信は
//! フロントエンドが命令境界で渡す（入力として記録される）。仮想時間を持たない。
//! 10Mbit/s の線の速さ・衝突・FIFO の過不足は再現しない。

use super::pd6710::{Card, Space};
use crate::snapshot::{Decoder, Encoder, Error};

/// 構成レジスタの属性メモリ上の位置（CIS の CISTPL_CONFIG に書く値）。
const CONFIG_BASE: u32 = 0x200;

/// カードのバッファメモリ（DP8390 のローカル DMA が見る空間のうち RAM の範囲）。
/// ne2000.dll は送信バッファを 4000h〜、受信リングを 4C00h〜7FFFh に置き、
/// 局アドレスを 0000h〜 からリモート DMA で読む（2026-09-30 観察）。NE2000 の
/// 慣習どおり 16KB の RAM を 4000h〜7FFFh に置く。
const RAM_BASE: usize = 0x4000;
const RAM_SIZE: usize = 0x4000;
/// 局アドレスの PROM（0000h〜001Fh）。ドライバは 8 ビットのカード（DCR の WTS=0）
/// として 12 バイトを読み、偶数番目のバイトを局アドレスに使う（奇数番目は読み捨てる。
/// 2026-09-30 観察）。各バイトを 2 回ずつ並べる。
/// TODO: 000Ch 以降（NE2000 では識別の値があるとされる）の中身は一次資料がなく 0 にしている。
const PROM_SIZE: usize = 0x20;

// Command Register（DS 10.3）
const CR_STP: u8 = 0x01;
const CR_STA: u8 = 0x02;
const CR_TXP: u8 = 0x04;
const CR_RD_MASK: u8 = 0x38;
const CR_RD_READ: u8 = 0x08;
const CR_RD_WRITE: u8 = 0x10;
const CR_RD_SEND: u8 = 0x18;
const CR_RD_ABORT: u8 = 0x20;
// Interrupt Status Register（DS 10.3）
const ISR_PRX: u8 = 0x01;
const ISR_PTX: u8 = 0x02;
const ISR_OVW: u8 = 0x10;
const ISR_CNT: u8 = 0x20;
const ISR_RDC: u8 = 0x40;
const ISR_RST: u8 = 0x80;
// Data Configuration Register（DS 10.3）
const DCR_LAS: u8 = 0x04;
// Receive Configuration Register（DS 10.3）
const RCR_AR: u8 = 0x02;
const RCR_AB: u8 = 0x04;
const RCR_AM: u8 = 0x08;
const RCR_PRO: u8 = 0x10;
const RCR_MON: u8 = 0x20;
// Receive Status Register（DS 10.3）
const RSR_PRX: u8 = 0x01;
const RSR_MPA: u8 = 0x10;
const RSR_PHY: u8 = 0x20;
// Transmit Status Register（DS 10.3）
const TSR_PTX: u8 = 0x01;

/// 既定の局アドレス（ローカル管理のユニキャスト 02:xx。"CRLN" の ASCII を入れた独自の値）。
pub const DEFAULT_MAC: [u8; 6] = [0x02, 0x43, 0x52, 0x4C, 0x4E, 0x01];

/// 1 フレームの上限（宛先〜データ。FCS を除く）。これを超える受信は捨てる。
pub const MAX_FRAME: usize = 1514;
/// ホストへの送信の列に溜めるフレーム数の上限（フロントエンドが取り出さないときに
/// メモリを使い切らないため。超えたら古いものから捨てる）。
const TX_QUEUE_MAX: usize = 256;

/// NE2000 互換の PC カード。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ne2000 {
    /// 局アドレス（PROM の中身。カードごとに固定）
    pub(crate) mac: [u8; 6],
    pub(crate) powered: bool,
    pub(crate) reset: bool,
    /// Configuration Option Register（SRESET・LevlREQ・構成の番号）
    pub(crate) cor: u8,
    /// Card Configuration and Status の書ける部分
    pub(crate) ccsr: u8,

    // DP8390 のレジスタ（DS 10.2）
    pub(crate) cr: u8,
    pub(crate) pstart: u8,
    pub(crate) pstop: u8,
    pub(crate) bnry: u8,
    pub(crate) tpsr: u8,
    pub(crate) tbcr: u16,
    pub(crate) isr: u8,
    pub(crate) rsar: u16,
    pub(crate) rbcr: u16,
    /// Current Remote DMA Address
    pub(crate) crda: u16,
    pub(crate) rcr: u8,
    pub(crate) tcr: u8,
    pub(crate) dcr: u8,
    pub(crate) imr: u8,
    pub(crate) par: [u8; 6],
    pub(crate) curr: u8,
    pub(crate) mar: [u8; 8],
    pub(crate) tsr: u8,
    pub(crate) ncr: u8,
    pub(crate) rsr: u8,
    /// Current Local DMA Address（受信・送信の後の位置）
    pub(crate) clda: u16,
    /// Network Tally Counters（CNTR0〜2。読むと 0 になる）
    pub(crate) cntr: [u8; 3],
    /// バッファメモリの RAM（RAM_BASE〜）
    pub(crate) mem: Vec<u8>,
    /// ホストへ送るフレーム（送信した順。フロントエンドが取り出す）
    pub(crate) tx: Vec<Vec<u8>>,
}

impl Ne2000 {
    pub const STATE_VERSION: u16 = 1;

    pub fn new(mac: [u8; 6]) -> Ne2000 {
        let mut n = Ne2000 {
            mac,
            powered: false,
            reset: true,
            cor: 0,
            ccsr: 0,
            cr: 0,
            pstart: 0,
            pstop: 0,
            bnry: 0,
            tpsr: 0,
            tbcr: 0,
            isr: 0,
            rsar: 0,
            rbcr: 0,
            crda: 0,
            rcr: 0,
            tcr: 0,
            dcr: 0,
            imr: 0,
            par: [0; 6],
            curr: 0,
            mar: [0; 8],
            tsr: 0,
            ncr: 0,
            rsr: 0,
            clda: 0,
            cntr: [0; 3],
            mem: vec![0; RAM_SIZE],
            tx: Vec::new(),
        };
        n.hard_reset();
        n
    }

    /// 局アドレス。
    pub fn mac(&self) -> [u8; 6] {
        self.mac
    }

    /// 送信されたフレームを取り出す（送信した順）。
    pub fn take_tx(&mut self) -> Vec<Vec<u8>> {
        std::mem::take(&mut self.tx)
    }

    /// PC カードのリセット（RESET ピン・SRESET・電源投入）。構成レジスタと
    /// DP8390 を初期化する。
    fn hard_reset(&mut self) {
        self.cor &= 0x80;
        self.ccsr = 0;
        self.nic_reset();
    }

    /// DP8390 のリセット（DS 11.0 の表: CR は TXP・STA が 0、RD2・STP が 1。ISR は
    /// RST が 1。IMR は全 0。DCR は LAS が 1。TCR は LB1・LB0 が 0）。表にない
    /// レジスタは値を保つ。
    fn nic_reset(&mut self) {
        self.cr = self.cr & !(CR_TXP | CR_STA) | CR_RD_ABORT | CR_STP;
        self.isr |= ISR_RST;
        self.imr = 0;
        self.dcr |= DCR_LAS;
        self.tcr &= !0x06;
    }

    fn configured(&self) -> bool {
        self.powered && !self.reset && self.cor & 0x80 == 0 && self.cor & 0x3F != 0
    }

    /// INT ピン（DS 10.3: IMR で許された ISR のビットが 1 つでも立っている間）。
    fn irq_line(&self) -> bool {
        self.isr & self.imr & 0x7F != 0
    }

    fn page(&self) -> u8 {
        self.cr >> 6
    }

    fn running(&self) -> bool {
        self.cr & CR_STA != 0 && self.cr & CR_STP == 0
    }

    /// ローカルのバスの読み出し（PROM・RAM）。
    /// TODO: どちらでもない範囲の読み値（実機では未接続かミラー）は一次資料がない。0 にしている。
    fn mem_read(&self, a: u16) -> u8 {
        let a = a as usize;
        if a < PROM_SIZE {
            if a < 12 { self.mac[a / 2] } else { 0 }
        } else if (RAM_BASE..RAM_BASE + RAM_SIZE).contains(&a) {
            self.mem[a - RAM_BASE]
        } else {
            0
        }
    }

    fn mem_write(&mut self, a: u16, v: u8) {
        let a = a as usize;
        if (RAM_BASE..RAM_BASE + RAM_SIZE).contains(&a) {
            self.mem[a - RAM_BASE] = v;
        }
    }

    // ---- 属性メモリ ----

    fn attr_read(&self, addr: u32) -> u8 {
        if addr & 1 != 0 {
            return 0;
        }
        if addr >= CONFIG_BASE {
            return match addr - CONFIG_BASE {
                0 => self.cor,
                2 => self.ccsr & 0x7C | if self.irq_line() { 0x02 } else { 0 },
                _ => 0,
            };
        }
        CIS.get((addr / 2) as usize).copied().unwrap_or(0xFF)
    }

    fn attr_write(&mut self, addr: u32, v: u8) {
        if addr & 1 != 0 || addr < CONFIG_BASE {
            return;
        }
        match addr - CONFIG_BASE {
            0 => {
                let was_sreset = self.cor & 0x80 != 0;
                self.cor = v;
                if v & 0x80 != 0 {
                    self.hard_reset();
                } else if was_sreset {
                    self.cor = 0;
                    self.hard_reset();
                }
            }
            2 => self.ccsr = v & 0x7C,
            _ => {}
        }
    }

    // ---- I/O ----

    fn io_read8(&mut self, port: u32) -> u8 {
        match port & 0x1F {
            0x10..=0x17 => self.data_read_byte(),
            0x18..=0x1F => {
                // TODO: リセットポートの動作（AX Tab-14 は「Reset」とだけ書く）。
                // 読み出しで DP8390 をリセットすることにしている。読み値は 0。
                self.nic_reset();
                0
            }
            r => self.reg_read(r as u8),
        }
    }

    fn io_write8(&mut self, port: u32, v: u8) {
        match port & 0x1F {
            0x10..=0x17 => self.data_write_byte(v),
            0x18..=0x1F => {} // TODO: リセットポートへの書き込み（AX は Reserved）
            r => self.reg_write(r as u8, v),
        }
    }

    fn reg_read(&mut self, r: u8) -> u8 {
        if r == 0 {
            return self.cr;
        }
        match (self.page(), r) {
            (0, 0x01) => self.clda as u8,
            (0, 0x02) => (self.clda >> 8) as u8,
            (0, 0x03) => self.bnry,
            (0, 0x04) => self.tsr,
            (0, 0x05) => self.ncr,
            (0, 0x06) => 0, // TODO: FIFO（ループバックの診断用）
            (0, 0x07) => self.isr,
            (0, 0x08) => self.crda as u8,
            (0, 0x09) => (self.crda >> 8) as u8,
            (0, 0x0C) => self.rsr,
            (0, 0x0D..=0x0F) => {
                let i = (r - 0x0D) as usize;
                std::mem::take(&mut self.cntr[i])
            }
            (1, 0x01..=0x06) => self.par[(r - 1) as usize],
            (1, 0x07) => self.curr,
            (1, 0x08..=0x0F) => self.mar[(r - 8) as usize],
            (2, 0x01) => self.pstart,
            (2, 0x02) => self.pstop,
            (2, 0x04) => self.tpsr,
            (2, 0x0C) => self.rcr | 0xC0,
            (2, 0x0D) => self.tcr | 0xE0,
            (2, 0x0E) => self.dcr | 0x80,
            (2, 0x0F) => self.imr | 0x80,
            // TODO: 予約・ページ 2 の診断用・ページ 3 の読み値は DS に記載がない
            _ => 0,
        }
    }

    fn reg_write(&mut self, r: u8, v: u8) {
        if r == 0 {
            self.cmd_write(v);
            return;
        }
        match (self.page(), r) {
            (0, 0x01) => self.pstart = v,
            (0, 0x02) => self.pstop = v,
            (0, 0x03) => self.bnry = v,
            (0, 0x04) => self.tpsr = v,
            (0, 0x05) => self.tbcr = self.tbcr & 0xFF00 | v as u16,
            (0, 0x06) => self.tbcr = self.tbcr & 0x00FF | (v as u16) << 8,
            // 1 を書いたビットを消す。RST は書いても変わらない（DS 10.3）
            (0, 0x07) => self.isr &= !(v & 0x7F),
            (0, 0x08) => {
                self.rsar = self.rsar & 0xFF00 | v as u16;
                self.crda = self.rsar;
            }
            (0, 0x09) => {
                self.rsar = self.rsar & 0x00FF | (v as u16) << 8;
                self.crda = self.rsar;
            }
            (0, 0x0A) => self.rbcr = self.rbcr & 0xFF00 | v as u16,
            (0, 0x0B) => self.rbcr = self.rbcr & 0x00FF | (v as u16) << 8,
            (0, 0x0C) => self.rcr = v & 0x3F,
            (0, 0x0D) => self.tcr = v & 0x1F,
            (0, 0x0E) => self.dcr = v & 0x7F,
            (0, 0x0F) => self.imr = v & 0x7F,
            (1, 0x01..=0x06) => self.par[(r - 1) as usize] = v,
            (1, 0x07) => self.curr = v,
            (1, 0x08..=0x0F) => self.mar[(r - 8) as usize] = v,
            (2, 0x01) => self.clda = self.clda & 0xFF00 | v as u16,
            (2, 0x02) => self.clda = self.clda & 0x00FF | (v as u16) << 8,
            // TODO: ページ 2 のその他（診断用）・ページ 3 は無視している
            _ => {}
        }
    }

    fn cmd_write(&mut self, v: u8) {
        let was_running = self.running();
        // TXP は 1 を書いたときだけ意味を持ち、0 を書いても変わらない（DS 10.3）
        self.cr = v & !CR_TXP | self.cr & CR_TXP;
        if v & CR_STP != 0 {
            // ソフトウェアリセット: 進行中の送受信は即座に終わっている扱い
            self.isr |= ISR_RST;
            if was_running {
                // DS 10.3 の注: 動作中に STP を立てると STP・STA の両方が残る
                self.cr |= CR_STA;
            }
        } else if v & CR_STA != 0 {
            self.isr &= !ISR_RST;
        }
        match v & CR_RD_MASK {
            CR_RD_READ | CR_RD_WRITE => {
                self.crda = self.rsar;
                if self.rbcr == 0 {
                    // バイト数 0 の DMA は即座に完了する
                    self.isr |= ISR_RDC;
                }
            }
            CR_RD_SEND => {
                // Send Packet（DS 9.0）: BNRY の受信パケットを読み出す
                self.crda = (self.bnry as u16) << 8;
                let a = self.crda;
                self.rbcr = u16::from_le_bytes([
                    self.mem_read(a.wrapping_add(2)),
                    self.mem_read(a.wrapping_add(3)),
                ]);
            }
            _ => {}
        }
        if v & CR_TXP != 0 && self.cr & CR_STA != 0 && self.cr & CR_STP == 0 {
            self.transmit();
        }
    }

    // ---- リモート DMA（データポート）----

    fn dma_active(&self) -> u8 {
        let rd = self.cr & CR_RD_MASK;
        if rd & CR_RD_ABORT != 0 { 0 } else { rd }
    }

    fn data_read_byte(&mut self) -> u8 {
        if !matches!(self.dma_active(), CR_RD_READ | CR_RD_SEND) || self.rbcr == 0 {
            return 0; // TODO: DMA が動いていないときのデータポートの読み値
        }
        let v = self.mem_read(self.crda);
        self.dma_step();
        v
    }

    fn data_write_byte(&mut self, v: u8) {
        if self.dma_active() != CR_RD_WRITE || self.rbcr == 0 {
            return;
        }
        self.mem_write(self.crda, v);
        self.dma_step();
    }

    /// リモート DMA を 1 バイト進める。PSTOP に達したら PSTART に戻る（DS 9.0 の
    /// Send Packet の説明。TODO: 通常の Remote Read でも戻るかは DS に明記がない）。
    fn dma_step(&mut self) {
        self.crda = self.crda.wrapping_add(1);
        if self.pstop != 0 && self.crda == (self.pstop as u16) << 8 {
            self.crda = (self.pstart as u16) << 8;
        }
        self.rbcr -= 1;
        if self.rbcr == 0 {
            if self.dma_active() == CR_RD_SEND {
                // 読み終えたパケットの次のページへ境界を進める（DS 9.0）
                // TODO: 実機は読み出し開始時にヘッダの次ページを覚える
                self.bnry = self.crda.div_ceil(0x100) as u8;
            }
            self.isr |= ISR_RDC;
        }
    }

    // ---- 送信 ----

    fn transmit(&mut self) {
        let start = (self.tpsr as usize) << 8;
        let len = self.tbcr as usize;
        let frame: Vec<u8> = (0..len)
            .map(|i| self.mem_read((start + i) as u16))
            .collect();
        self.clda = (start + len) as u16;
        self.ncr = 0;
        self.tsr = TSR_PTX;
        self.isr |= ISR_PTX;
        if self.tcr & 0x06 != 0 {
            // TODO: ループバック（DS 12.0）は線に出さない。受信側・FIFO の動作は未実装
            return;
        }
        if self.tx.len() >= TX_QUEUE_MAX {
            self.tx.remove(0);
        }
        self.tx.push(frame);
    }

    // ---- 受信 ----

    /// 宛先が受け取る対象か（DS 5.0・10.3 の RCR・10.9 のマルチキャストのハッシュ）。
    fn accepts(&self, dst: &[u8]) -> (bool, bool) {
        let multicast = dst[0] & 1 != 0;
        if dst == [0xFF; 6] {
            return (self.rcr & RCR_AB != 0, true);
        }
        if multicast {
            if self.rcr & RCR_AM == 0 {
                return (false, true);
            }
            let bit = crc32_be_top6(dst);
            return (self.mar[(bit >> 3) as usize] & (1 << (bit & 7)) != 0, true);
        }
        (self.rcr & RCR_PRO != 0 || dst == self.par, false)
    }

    /// 線からフレームを受け取る（宛先〜データ。FCS は付けずに渡す）。受信バッファに
    /// 入れたら true。受信できる状態でない・宛先が合わない・空きがないときは false。
    pub fn receive(&mut self, frame: &[u8]) -> bool {
        if !self.configured() || !self.running() || self.tcr & 0x06 != 0 {
            return false;
        }
        if frame.len() < 14 || frame.len() > MAX_FRAME {
            return false;
        }
        let (ok, multi) = self.accepts(&frame[..6]);
        if !ok {
            return false;
        }
        if self.rcr & RCR_MON != 0 {
            self.rsr = RSR_MPA;
            self.bump_counter(2);
            return false;
        }
        // 線上のフレームは最小 60 バイト（パディング）＋ FCS 4 バイト（DS 5.0）。
        // 受信バイト数は FCS を含む（DS 4.0: SFD の後のバイトごとに数える）。
        let mut data = frame.to_vec();
        if data.len() < 60 {
            // 送り手（ホスト側のスタック）が短いフレームを渡しても実機の線と同じ形にする
            data.resize(60, 0);
        }
        let _ = RCR_AR; // 短いフレームは詰めて渡すので Runt は起きない
        data.extend_from_slice(&crc32(&data).to_le_bytes());
        let pages = (data.len() + 4).div_ceil(256); // ヘッダ 4 バイト込み
        let (ps, pe) = (self.pstart, self.pstop);
        if ps >= pe || self.curr < ps || self.curr >= pe {
            return false; // TODO: 受信リングの設定が不正なときの動作
        }
        // 先頭のページから順にページをつなぐ。次のページが BNRY に当たったら受信を
        // 中断する（DS 7.0 の Linking Receive Buffer Pages・Buffer Ring Overflow）。
        let wrap = |p: u8| if p.wrapping_add(1) >= pe { ps } else { p + 1 };
        let mut last = self.curr;
        for _ in 1..pages {
            let n = wrap(last);
            if n == self.bnry {
                self.rsr = RSR_MPA;
                self.isr |= ISR_OVW;
                self.bump_counter(2);
                return false;
            }
            last = n;
        }
        let next = wrap(last);
        self.rsr = RSR_PRX | if multi { RSR_PHY } else { 0 };
        // 受信バイト数は FCS を含み、ヘッダの 4 バイトを含まない（DS 4.0: SFD の後の
        // バイトを数える）。ne2000.dll は「受信バイト数 − 4」をフレームの長さとして
        // 60〜1514 の範囲を確かめる（2026-09-30 観察）ので、この解釈と合う。
        let count = (data.len() as u16).to_le_bytes();
        let header = [self.rsr, next, count[0], count[1]];
        let mut a = (self.curr as usize) << 8;
        for &b in header.iter().chain(data.iter()) {
            self.mem_write(a as u16, b);
            a += 1;
            if a == (pe as usize) << 8 {
                a = (ps as usize) << 8;
            }
        }
        self.clda = a as u16;
        self.curr = next;
        self.isr |= ISR_PRX;
        true
    }

    fn bump_counter(&mut self, i: usize) {
        // 最大は 192（C0h）。最上位ビットが立ったら CNT（DS 10.10）
        if self.cntr[i] < 0xC0 {
            self.cntr[i] += 1;
        }
        if self.cntr[i] & 0x80 != 0 {
            self.isr |= ISR_CNT;
        }
    }

    // ---- スナップショット ----

    pub fn save_state(&self, e: &mut Encoder) {
        let Ne2000 {
            mac,
            powered,
            reset,
            cor,
            ccsr,
            cr,
            pstart,
            pstop,
            bnry,
            tpsr,
            tbcr,
            isr,
            rsar,
            rbcr,
            crda,
            rcr,
            tcr,
            dcr,
            imr,
            par,
            curr,
            mar,
            tsr,
            ncr,
            rsr,
            clda,
            cntr,
            mem,
            tx,
        } = self;
        e.bytes(mac);
        e.bool(*powered);
        e.bool(*reset);
        for v in [*cor, *ccsr, *cr, *pstart, *pstop, *bnry, *tpsr] {
            e.u8(v);
        }
        e.u16(*tbcr);
        e.u8(*isr);
        e.u16(*rsar);
        e.u16(*rbcr);
        e.u16(*crda);
        for v in [*rcr, *tcr, *dcr, *imr] {
            e.u8(v);
        }
        e.bytes(par);
        e.u8(*curr);
        e.bytes(mar);
        for v in [*tsr, *ncr, *rsr] {
            e.u8(v);
        }
        e.u16(*clda);
        e.bytes(cntr);
        e.bytes(mem);
        e.u32(tx.len() as u32);
        for f in tx {
            e.bytes(f);
        }
    }

    pub fn load_state(d: &mut Decoder) -> Result<Ne2000, Error> {
        fn arr<const N: usize>(d: &mut Decoder) -> Result<[u8; N], Error> {
            let b = d.bytes()?;
            match b.try_into() {
                Ok(a) => Ok(a),
                Err(_) => d.err("bad array length"),
            }
        }
        let mac = arr::<6>(d)?;
        let mut n = Ne2000::new(mac);
        n.powered = d.bool()?;
        n.reset = d.bool()?;
        n.cor = d.u8()?;
        n.ccsr = d.u8()?;
        n.cr = d.u8()?;
        n.pstart = d.u8()?;
        n.pstop = d.u8()?;
        n.bnry = d.u8()?;
        n.tpsr = d.u8()?;
        n.tbcr = d.u16()?;
        n.isr = d.u8()?;
        n.rsar = d.u16()?;
        n.rbcr = d.u16()?;
        n.crda = d.u16()?;
        n.rcr = d.u8()?;
        n.tcr = d.u8()?;
        n.dcr = d.u8()?;
        n.imr = d.u8()?;
        n.par = arr::<6>(d)?;
        n.curr = d.u8()?;
        n.mar = arr::<8>(d)?;
        n.tsr = d.u8()?;
        n.ncr = d.u8()?;
        n.rsr = d.u8()?;
        n.clda = d.u16()?;
        n.cntr = arr::<3>(d)?;
        let mem = d.bytes()?;
        if mem.len() != RAM_SIZE {
            return d.err("bad buffer memory size");
        }
        n.mem = mem.to_vec();
        let count = d.u32()?;
        if count as usize > TX_QUEUE_MAX {
            return d.err("too many queued frames");
        }
        n.tx = Vec::new();
        for _ in 0..count {
            let f = d.bytes()?;
            if f.len() > 0x1_0000 {
                return d.err("queued frame too large");
            }
            n.tx.push(f.to_vec());
        }
        Ok(n)
    }
}

impl Card for Ne2000 {
    fn read(&mut self, space: Space, addr: u32, wide: bool) -> u16 {
        if !self.powered || self.reset || self.cor & 0x80 != 0 {
            return 0;
        }
        match space {
            Space::Attr => self.attr_read(addr) as u16,
            Space::Common => 0, // 共通メモリは持たない
            Space::Io => {
                if !self.configured() {
                    return 0;
                }
                if wide {
                    let lo = self.io_read8(addr) as u16;
                    let hi = self.io_read8(addr + 1) as u16;
                    lo | hi << 8
                } else {
                    self.io_read8(addr) as u16
                }
            }
        }
    }

    fn write(&mut self, space: Space, addr: u32, wide: bool, v: u16) {
        if !self.powered || self.reset {
            return;
        }
        match space {
            Space::Attr => self.attr_write(addr, v as u8),
            Space::Common => {}
            Space::Io => {
                if !self.configured() {
                    return;
                }
                self.io_write8(addr, v as u8);
                if wide {
                    self.io_write8(addr + 1, (v >> 8) as u8);
                }
            }
        }
    }

    fn rdy_ireq(&self) -> bool {
        if self.cor & 0x3F == 0 {
            // メモリのインタフェース: RDY
            self.powered && !self.reset
        } else {
            // TODO: パルスモード（COR の LevlREQ=0）はレベルとして出している
            self.configured() && self.irq_line()
        }
    }

    fn iois16(&self, addr: u32) -> bool {
        // データポートだけが 16 ビット（AX Tab-14。レジスタは 8 ビット）
        (0x10..0x18).contains(&(addr & 0x1F))
    }

    fn set_reset(&mut self, reset: bool) {
        if reset && !self.reset {
            self.cor = 0;
            self.hard_reset();
        }
        self.reset = reset;
    }

    fn set_power(&mut self, on: bool) {
        self.powered = on;
        if !on {
            self.cor = 0;
            self.hard_reset();
            self.reset = true;
        }
    }
}

/// イーサネットの FCS（CRC-32、AUTODIN II の多項式。DS 5.0）。ビットは下位から送る
/// ので反転形（0xEDB88320）で計算し、線上の順（下位バイトから）に並べる。
fn crc32(data: &[u8]) -> u32 {
    let mut c = 0xFFFF_FFFFu32;
    for &b in data {
        c ^= b as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 {
                c >> 1 ^ 0xEDB8_8320
            } else {
                c >> 1
            };
        }
    }
    !c
}

/// マルチキャストのハッシュ（DS 10.9: 宛先アドレスの最後のビットが CRC に入った時点の
/// CRC 生成器の上位 6 ビット）。
/// TODO: 「上位 6 ビット」を生成器のどのビット順で読むかは DS の図（TL/F/11157-53）が
/// テキストにならず未確認。ドライバがマルチキャストを使ったら見直す。
fn crc32_be_top6(dst: &[u8]) -> u8 {
    let mut c = 0xFFFF_FFFFu32;
    for &b in dst {
        let mut b = b;
        for _ in 0..8 {
            let fb = (c >> 31) ^ (b as u32 & 1);
            c <<= 1;
            if fb != 0 {
                c ^= 0x04C1_1DB7;
            }
            b >>= 1;
        }
    }
    (c >> 26) as u8
}

/// CIS（属性メモリの偶数番地）。書式は cf.rs と同じく SanDisk Table 6-1 の説明に従う。
/// 製造者・製品は独自の値。
/// この CIS で PCMCIA の検出の表の NE2000（DetectNE2000）が名乗り出て ne2000.dll が
/// 読み込まれ、構成 1・I/O 窓 300h〜31Fh・COR=41h（レベル割り込み）・CCSR の IOis8 と
/// 設定される（2026-09-30 観察）。
/// TODO: CISTPL_FUNCID の機能コード 06h（ネットワーク）は手元の一次資料に記載がなく、
/// DetectNE2000 がどのタプルを見て判断しているかも未確認。
#[rustfmt::skip]
static CIS: &[u8] = &[
    // CISTPL_DEVICE: I/O 型のデバイス（cf.rs と同じ）
    0x01, 0x04, 0xDF, 0x12, 0x01, 0xFF,
    // CISTPL_MANFID: 製造者 0000h・製品 0001h（独自の値）
    0x20, 0x04, 0x00, 0x00, 0x01, 0x00,
    // CISTPL_VERS_1: 版 4.1、製造者・製品・版の文字列
    0x15, 0x1C, 0x04, 0x01,
    b'C', b'E', b'r', b'u', b'l', b'e', b'a', b'n', 0x00,
    b'V', b'i', b'r', b't', b'u', b'a', b'l', b' ', b'N', b'I', b'C', 0x00,
    b'1', b'.', b'0', 0x00,
    0xFF,
    // CISTPL_FUNCID: ネットワーク（06h）
    0x21, 0x02, 0x06, 0x00,
    // CISTPL_CONFIG: 大きさの欄 01h（基底 2 バイト・マスク 1 バイト）、最後の構成 01h、
    // 構成レジスタは 200h、COR・CCSR あり
    0x1A, 0x05, 0x01, 0x01, 0x00, 0x02, 0x03,
    // CISTPL_CFTABLE_ENTRY 構成 1（既定・インタフェースの欄あり）: I/O のインタフェース。
    // VCC の電源（公称 5V のみ）、I/O は 10 本のアドレス線で範囲 1 個（300h〜31Fh）の
    // 8/16 ビット、割り込みはレベル・IRQ 0〜15 のマスク
    0x1B, 0x0D, 0xC1, 0x41, 0x19, 0x01, 0x55, 0xEA, 0x60, 0x00, 0x03, 0x1F,
    0x30, 0xFF, 0xFF,
    // CISTPL_NO_LINK
    0x14, 0x00,
    // CISTPL_END
    0xFF,
];

#[cfg(test)]
mod tests {
    use super::*;

    const MAC: [u8; 6] = [0x02, 0x11, 0x22, 0x33, 0x44, 0x55];

    fn io_w(n: &mut Ne2000, port: u32, v: u8) {
        n.write(Space::Io, port, false, v as u16);
    }

    fn io_r(n: &mut Ne2000, port: u32) -> u8 {
        n.read(Space::Io, port, false) as u8
    }

    /// 電源を入れて構成 1 にし、ne2000.dll と同じ手順（DS 11.0）で初期化したカード。
    /// 送信バッファ 40h〜4Bh、受信リング 4Ch〜80h。
    fn started() -> Ne2000 {
        let mut n = Ne2000::new(MAC);
        n.set_power(true);
        n.set_reset(false);
        n.write(Space::Attr, CONFIG_BASE, false, 0x41);
        for (r, v) in [
            (0x00, 0x21),
            (0x0E, 0x48),
            (0x0A, 0),
            (0x0B, 0),
            (0x0C, 0x04),
            (0x0D, 0x02),
            (0x01, 0x4C),
            (0x02, 0x80),
            (0x03, 0x4C),
            (0x07, 0xFF),
            (0x0F, 0x1B),
            (0x00, 0x61),
        ] {
            io_w(&mut n, r, v);
        }
        for (i, &b) in MAC.iter().enumerate() {
            io_w(&mut n, 1 + i as u32, b);
        }
        io_w(&mut n, 0x07, 0x4D); // CURR
        io_w(&mut n, 0x00, 0x22);
        io_w(&mut n, 0x0D, 0x00);
        n
    }

    fn remote_read(n: &mut Ne2000, addr: u16, len: u16) -> Vec<u8> {
        io_w(n, 0x08, addr as u8);
        io_w(n, 0x09, (addr >> 8) as u8);
        io_w(n, 0x0A, len as u8);
        io_w(n, 0x0B, (len >> 8) as u8);
        io_w(n, 0x00, 0x0A);
        (0..len).map(|_| io_r(n, 0x10)).collect()
    }

    #[test]
    fn reset_state_and_prom() {
        let mut n = started();
        io_w(&mut n, 0x07, 0xFF);
        let prom = remote_read(&mut n, 0, 12);
        let want: Vec<u8> = MAC.iter().flat_map(|&b| [b, b]).collect();
        assert_eq!(prom, want);
        assert_eq!(io_r(&mut n, 0x07) & ISR_RDC, ISR_RDC);
        // リセットポートを読むと DP8390 がリセット状態になる（CR の STP・ISR の RST）
        io_r(&mut n, 0x1F);
        assert_eq!(
            io_r(&mut n, 0x00) & (CR_STP | CR_RD_ABORT),
            CR_STP | CR_RD_ABORT
        );
        assert_eq!(io_r(&mut n, 0x07) & ISR_RST, ISR_RST);
    }

    #[test]
    fn transmit_via_remote_write() {
        let mut n = started();
        let frame: Vec<u8> = (0..70u8).collect();
        io_w(&mut n, 0x08, 0x00);
        io_w(&mut n, 0x09, 0x40);
        io_w(&mut n, 0x0A, frame.len() as u8);
        io_w(&mut n, 0x0B, 0);
        io_w(&mut n, 0x00, 0x12);
        for &b in &frame {
            io_w(&mut n, 0x10, b);
        }
        io_w(&mut n, 0x04, 0x40);
        io_w(&mut n, 0x05, frame.len() as u8);
        io_w(&mut n, 0x06, 0);
        io_w(&mut n, 0x00, 0x26);
        assert_eq!(n.take_tx(), vec![frame]);
        assert_eq!(io_r(&mut n, 0x07) & (ISR_PTX | ISR_RDC), ISR_PTX | ISR_RDC);
        assert_eq!(io_r(&mut n, 0x04), TSR_PTX);
        assert!(n.rdy_ireq(), "PTX is enabled in IMR");
        io_w(&mut n, 0x07, 0xFF);
        assert!(!n.rdy_ireq());
    }

    #[test]
    fn receive_into_ring_with_wrap_and_overflow() {
        let mut n = started();
        let mut frame = MAC.to_vec();
        frame.extend_from_slice(&[0x02, 0, 0, 0, 0, 1, 0x08, 0x00]);
        frame.resize(600, 0xAB); // 3 ページ（ヘッダ・FCS 込み 608 バイト）
        let mut expect_page = 0x4Du8;
        for i in 0..20 {
            // BNRY を読んだ位置の手前に保つ（ドライバの慣習: 次に読むページ − 1）
            let ok = n.receive(&frame);
            if !ok {
                assert!(i > 0);
                assert_eq!(io_r(&mut n, 0x07) & ISR_OVW, ISR_OVW);
                break;
            }
            let hdr = remote_read(&mut n, (expect_page as u16) << 8, 4);
            assert_eq!(hdr[0], RSR_PRX);
            assert_eq!(u16::from_le_bytes([hdr[2], hdr[3]]), 604);
            let next = hdr[1];
            let want_next = if expect_page + 3 >= 0x80 {
                expect_page + 3 - 0x80 + 0x4C
            } else {
                expect_page + 3
            };
            assert_eq!(next, want_next, "packet {i}");
            // 中身（リングの終わりをまたいでも続けて読める）
            let body = remote_read(&mut n, ((expect_page as u16) << 8) + 4, 600);
            assert_eq!(body, frame);
            expect_page = next;
            if i < 10 {
                // 読み終えたら境界を進める
                io_w(&mut n, 0x03, if next == 0x4C { 0x7F } else { next - 1 });
            }
        }
        // 宛先の違うユニキャストは受け取らない
        io_w(&mut n, 0x03, 0x4C);
        let mut other = frame.clone();
        other[5] ^= 1;
        assert!(!n.receive(&other));
    }

    #[test]
    fn state_round_trip() {
        let mut n = started();
        let mut frame = vec![0xFF; 6];
        frame.extend_from_slice(&[0x02, 0, 0, 0, 0, 1, 0x08, 0x06]);
        frame.resize(42, 7);
        assert!(n.receive(&frame));
        let mut w = crate::snapshot::Writer::new(Vec::new(), "t", "").unwrap();
        w.chunk("ne2000", Ne2000::STATE_VERSION, |e| n.save_state(e))
            .unwrap();
        let buf = w.finish().unwrap();
        let mut r = crate::snapshot::Reader::new(&buf[..]).unwrap();
        let c = r.expect("ne2000").unwrap();
        let mut d = c.decoder(Ne2000::STATE_VERSION).unwrap();
        let m = Ne2000::load_state(&mut d).unwrap();
        d.finish().unwrap();
        assert_eq!(m, n);
    }

    #[test]
    fn cis_tuple_chain() {
        let mut i = 0;
        while CIS[i] != 0xFF {
            let link = CIS[i + 1] as usize;
            i += 2 + link;
            assert!(i < CIS.len(), "tuple overruns the CIS");
        }
        assert_eq!(i, CIS.len() - 1);
    }
}
