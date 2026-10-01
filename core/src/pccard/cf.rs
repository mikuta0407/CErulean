//! CompactFlash ストレージカード（PC カードの ATA）。
//!
//! 一次資料:
//!   - CF+ and CompactFlash Specification Rev 3.0（CFA、2004。以下「CF」）:
//!     属性メモリ・構成レジスタ（4.4）、タスクファイル（6.1）、コマンド（6.2）
//!   - SanDisk CompactFlash Memory Card OEM Product Manual v1.0（2009。以下「SD」）
//!     Table 6-1: CIS の各バイトと各フィールドの意味。PCMCIA の Metaformat
//!     （タプルの書式）は非公開で、CF もそちらを参照するだけなので、実在のカードの
//!     CIS の記述を根拠にする。製造者 ID・文字列は独自の値にする（2026-09-29
//!     ユーザー決定）。
//!
//! モデル: コマンドは受け取った時点で完了する（BSY はゲストから見えない）。
//! 仮想時間を持たない（決定論的で、時間の同期が要らない）。
//! TODO: 実機のカードはコマンドの処理に時間がかかる（CF 6.2 の Class 1〜3）。
//! 即座の完了で困るドライバが現れたら時間を持たせる（next_event と board.rs の同期）。

use super::pd6710::{Card, Space};
use crate::snapshot::{Decoder, Encoder, Error};

pub const SECTOR: usize = 512;
/// ディスクイメージの上限（512MB。スナップショットのチャンクの上限・ブラウザの
/// メモリを考えた値）。
pub const MAX_SECTORS: u32 = 1 << 20;

/// 構成レジスタの属性メモリ上の位置（CF 4.4.3: CompactFlash ストレージカードは 200h。
/// CIS の CISTPL_CONFIG にも書く）。
const CONFIG_BASE: u32 = 0x200;

// ATA の状態（CF 6.1.5.8）
const ST_BSY: u8 = 0x80;
const ST_RDY: u8 = 0x40;
const ST_DSC: u8 = 0x10;
const ST_DRQ: u8 = 0x08;
const ST_ERR: u8 = 0x01;
// エラー（CF 6.1.5.1 の Error Register）
const ER_IDNF: u8 = 0x10;
const ER_ABRT: u8 = 0x04;

/// Read/Write Multiple の 1 ブロックの最大セクタ数（IDENTIFY の word 47）。
const MAX_MULTIPLE: u8 = 1;

/// データ転送の状態。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Xfer {
    None,
    /// ホストがバッファを読んでいる（Read Sectors 等）。left は残りのセクタ数
    /// （バッファ中のものを含む）、block はこの割り込みの単位の残り、unit は
    /// 割り込みの単位（Multiple なら設定値、他は 1）。media=false は IDENTIFY・
    /// Read Buffer（読み終えたら終わり）。
    Read {
        left: u32,
        block: u32,
        unit: u32,
        media: bool,
    },
    /// ホストがバッファに書いている。media=false は Write Buffer・Format Track。
    Write {
        left: u32,
        block: u32,
        unit: u32,
        media: bool,
    },
}

impl Xfer {
    fn encode(&self, e: &mut Encoder) {
        let (tag, left, block, unit, media) = match *self {
            Xfer::None => (0, 0, 0, 0, false),
            Xfer::Read {
                left,
                block,
                unit,
                media,
            } => (1, left, block, unit, media),
            Xfer::Write {
                left,
                block,
                unit,
                media,
            } => (2, left, block, unit, media),
        };
        e.u8(tag);
        e.u32(left);
        e.u32(block);
        e.u32(unit);
        e.bool(media);
    }

    fn decode(d: &mut Decoder) -> Result<Xfer, Error> {
        let (tag, left, block, unit, media) = (d.u8()?, d.u32()?, d.u32()?, d.u32()?, d.bool()?);
        if tag != 0 && (left == 0 || block == 0 || block > left || unit == 0) {
            return d.err("bad transfer state");
        }
        Ok(match tag {
            0 => Xfer::None,
            1 => Xfer::Read {
                left,
                block,
                unit,
                media,
            },
            2 => Xfer::Write {
                left,
                block,
                unit,
                media,
            },
            _ => return d.err("bad transfer state"),
        })
    }
}

/// CompactFlash ストレージカード。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CfCard {
    /// ディスクの中身（セクタ数 × 512 バイト。保存する）
    pub(crate) disk: Vec<u8>,
    pub(crate) powered: bool,
    pub(crate) reset: bool,

    // 構成レジスタ（CF 4.4.4〜4.4.7）
    /// Configuration Option Register（SRESET・LevlREQ・構成の番号）
    pub(crate) cor: u8,
    /// Card Configuration and Status の書ける部分（SigChg・IOis8・-XE・Audio・PwrDwn）
    pub(crate) ccsr: u8,
    /// Pin Replacement の CReady・CWProt（bit5・bit4）
    pub(crate) prr: u8,
    /// Socket and Copy
    pub(crate) scr: u8,

    // タスクファイル（CF 6.1.5）
    pub(crate) error: u8,
    pub(crate) feature: u8,
    pub(crate) count: u8,
    pub(crate) sector: u8,
    pub(crate) cyl_lo: u8,
    pub(crate) cyl_hi: u8,
    pub(crate) head: u8,
    pub(crate) status: u8,
    pub(crate) devctl: u8,
    /// 割り込み要求（INTRQ。Status の読み出しでクリア）
    pub(crate) intrq: bool,

    // 転送
    pub(crate) buf: Vec<u8>,
    pub(crate) buf_pos: u16,
    xfer: Xfer,
    /// 転送中のセクタの LBA
    pub(crate) lba: u32,
    /// Read/Write Multiple のブロックのセクタ数（0 = 無効）
    pub(crate) multiple: u8,
    /// 現在の変換の CHS（Initialize Drive Parameters で変わる）
    pub(crate) cur_heads: u8,
    pub(crate) cur_spt: u8,
}

impl CfCard {
    pub const STATE_VERSION: u16 = 1;

    /// ディスクイメージからカードを作る。大きさは 512 バイトの倍数で、1〜MAX_SECTORS
    /// セクタ。
    pub fn new(disk: Vec<u8>) -> Result<CfCard, String> {
        if disk.is_empty() || !disk.len().is_multiple_of(SECTOR) {
            return Err(format!(
                "card image size {} is not a positive multiple of 512",
                disk.len()
            ));
        }
        if (disk.len() / SECTOR) as u64 > MAX_SECTORS as u64 {
            return Err(format!(
                "card image is too large ({} bytes, limit {} MB)",
                disk.len(),
                (MAX_SECTORS as u64 * SECTOR as u64) >> 20
            ));
        }
        let mut c = CfCard {
            disk,
            powered: false,
            reset: true,
            cor: 0,
            ccsr: 0,
            prr: 0,
            scr: 0,
            error: 0,
            feature: 0,
            count: 0,
            sector: 0,
            cyl_lo: 0,
            cyl_hi: 0,
            head: 0,
            status: 0,
            devctl: 0,
            intrq: false,
            buf: vec![0; SECTOR],
            buf_pos: 0,
            xfer: Xfer::None,
            lba: 0,
            multiple: 0,
            cur_heads: 0,
            cur_spt: 0,
        };
        c.hard_reset();
        Ok(c)
    }

    pub fn disk(&self) -> &[u8] {
        &self.disk
    }

    pub fn into_disk(self) -> Vec<u8> {
        self.disk
    }

    fn sectors(&self) -> u32 {
        (self.disk.len() / SECTOR) as u32
    }

    /// 既定の変換（IDENTIFY の word 1・3・6）。ヘッド 16 以下・トラックあたり 32 セクタ、
    /// シリンダはなるべく 1024 以下になる最小のヘッド数（小さいカードでも
    /// シリンダが 1 以上になるように）。
    /// TODO: 実在のカードの変換は容量ごとの表（SD の IDENTIFY の表）。独自の式にしている。
    fn default_chs(&self) -> (u16, u8, u8) {
        let spt = 32u32;
        let total = self.sectors();
        let mut heads = 2u32;
        while heads < 16 && total / (heads * spt) > 1024 {
            heads *= 2;
        }
        let cyl = (total / (heads * spt)).clamp(1, 65535);
        (cyl as u16, heads as u8, spt as u8)
    }

    fn cur_cyls(&self) -> u32 {
        let hs = self.cur_heads as u32 * self.cur_spt as u32;
        self.sectors().checked_div(hs).map_or(0, |c| c.min(65535))
    }

    /// PC カードのリセット（RESET ピン・SRESET・電源投入。CF 4.4.4）。構成レジスタも
    /// 初期化する（メモリのインタフェース・構成 0）。
    fn hard_reset(&mut self) {
        self.cor &= 0x80; // SRESET ビット自体はホストが戻すまで残る（CF 4.4.4）
        self.ccsr = 0;
        self.prr = 0;
        self.scr = 0;
        self.devctl = 0;
        self.multiple = 0;
        let (_, h, s) = self.default_chs();
        self.cur_heads = h;
        self.cur_spt = s;
        self.ata_reset();
    }

    /// ATA のリセット後の状態（診断の結果 01h と、ATA の装置の署名）。
    /// TODO: CF には署名の値の記載がない。ATA の慣習（SC=1・SN=1・CL=CH=0）に
    /// している。
    fn ata_reset(&mut self) {
        self.error = 0x01;
        self.feature = 0;
        self.count = 1;
        self.sector = 1;
        self.cyl_lo = 0;
        self.cyl_hi = 0;
        self.head = 0;
        self.status = ST_RDY | ST_DSC;
        self.intrq = false;
        self.xfer = Xfer::None;
        self.buf_pos = 0;
    }

    fn active(&self) -> bool {
        self.powered && !self.reset && self.cor & 0x80 == 0
    }

    /// 構成の番号（CF Table 32）。
    fn conf(&self) -> u8 {
        self.cor & 0x3F
    }

    /// Device Control の -IEn が 0 なら割り込みを出す（CF 6.1.5.9）。
    fn irq_line(&self) -> bool {
        self.intrq && self.devctl & 0x02 == 0
    }

    // ---- 属性メモリ（CIS と構成レジスタ）----

    fn attr_read(&mut self, addr: u32) -> u8 {
        if addr & 1 != 0 {
            return 0; // 奇数番地は無効（CF 4.4.3）。TODO: 実機で読める値は不定
        }
        if addr >= CONFIG_BASE {
            return match addr - CONFIG_BASE {
                0 => self.cor,
                2 => {
                    // Changed（bit7）は Pin Replacement の CReady・CWProt のどちらか
                    let changed = if self.prr & 0x30 != 0 { 0x80 } else { 0 };
                    let int = if self.irq_line() { 0x02 } else { 0 };
                    changed | self.ccsr & 0x7C | int
                }
                // RReady（bit1）: 常に Ready。WProt（bit0）は 0。bit3:2 は 1（CF 4.4.6）
                4 => self.prr & 0x30 | 0x0C | 0x02,
                6 => self.scr,
                _ => 0,
            };
        }
        CIS.get((addr / 2) as usize).copied().unwrap_or(0xFF)
    }

    fn attr_write(&mut self, addr: u32, v: u8) {
        if addr & 1 != 0 || addr < CONFIG_BASE {
            return; // CIS への書き込みは無効（CF Table 29）
        }
        match addr - CONFIG_BASE {
            0 => {
                let was_sreset = self.cor & 0x80 != 0;
                self.cor = v;
                if v & 0x80 != 0 {
                    // SRESET: RESET ピンと同じ（CF 4.4.4）
                    self.hard_reset();
                } else if was_sreset {
                    self.cor = 0; // 戻すと構成なしのリセット後の状態（CF 4.4.4）
                    self.hard_reset();
                }
            }
            2 => self.ccsr = v & 0x7C,
            4 => {
                // CReady・CWProt は対応するマスクビットが 1 のときだけ書ける（CF Table 34）
                for (c, m) in [(0x20u8, 0x02u8), (0x10, 0x01)] {
                    if v & m != 0 {
                        self.prr = self.prr & !c | v & c;
                    }
                }
            }
            6 => self.scr = v & 0x7F,
            _ => {}
        }
    }

    // ---- タスクファイルの振り分け ----

    /// カードの空間のアドレスをタスクファイルの番号（0〜15）にする（CF 6.1.1〜6.1.3）。
    fn taskfile_offset(&self, space: Space, addr: u32) -> Option<u32> {
        match (space, self.conf()) {
            (Space::Common, 0) => {
                let a = addr & 0x7FF; // 2KB の窓（CF Table 42。A10 以下で選ぶ）
                Some(if a >= 0x400 { 8 | (a & 1) } else { a & 0xF })
            }
            (Space::Io, 1) => Some(addr & 0xF),
            (Space::Io, 2 | 3) => {
                let base = if self.conf() == 2 { 0x1F0 } else { 0x170 };
                let a = addr & 0x3FF;
                if (base..base + 8).contains(&a) {
                    Some(a - base)
                } else if a == base + 0x206 || a == base + 0x207 {
                    Some(a - base - 0x200 + 0x8) // 3F6h/3F7h → E/F
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    fn data_read_byte(&mut self) -> u8 {
        if self.status & ST_DRQ == 0 || !matches!(self.xfer, Xfer::Read { .. }) {
            return 0; // TODO: DRQ でないときのデータレジスタの値は CF に記載がない
        }
        let v = self.buf[self.buf_pos as usize];
        self.buf_pos += 1;
        if self.buf_pos as usize == SECTOR {
            self.read_sector_done();
        }
        v
    }

    fn data_write_byte(&mut self, v: u8) {
        if self.status & ST_DRQ == 0 || !matches!(self.xfer, Xfer::Write { .. }) {
            return;
        }
        self.buf[self.buf_pos as usize] = v;
        self.buf_pos += 1;
        if self.buf_pos as usize == SECTOR {
            self.write_sector_done();
        }
    }

    fn reg_read(&mut self, off: u32) -> u8 {
        // ドライブ 1 を選んでいるときは応答しない（CF 6.1.5.7: PCMCIA のモードでの
        // ドライブ 1 は廃止）。TODO: 応答しない場合の値は CF に記載がない（0）。
        if self.head & 0x10 != 0 && off != 0xE && off != 0x7 {
            return 0;
        }
        match off {
            0 | 8 | 9 => self.data_read_byte(),
            1 | 0xD => self.error,
            2 => self.count,
            3 => self.sector,
            4 => self.cyl_lo,
            5 => self.cyl_hi,
            6 => self.head,
            7 => {
                if self.head & 0x10 != 0 {
                    return 0;
                }
                self.intrq = false; // Status の読み出しは割り込みを取り下げる（CF 6.1.5.8）
                self.status
            }
            0xE => {
                if self.head & 0x10 != 0 {
                    return 0;
                }
                self.status
            }
            0xF => {
                // Drive Address（CF 6.1.5.10）: -WTG=1（書き込み中でない）、-HSn、
                // -nDS0=0（ドライブ 0 が選ばれている）
                let hs = !self.head & 0x0F;
                0x40 | hs << 2 | 0x02
            }
            _ => 0xFF, // 0xA〜0xC は割り当てなし。TODO: 実機の値は不明
        }
    }

    fn reg_write(&mut self, off: u32, v: u8) {
        if off == 0xE {
            self.devctl_write(v);
            return;
        }
        if self.status & ST_BSY != 0 {
            return; // BSY の間はコマンドブロックに書けない（CF 6.1.5.8）
        }
        match off {
            0 | 8 | 9 => self.data_write_byte(v),
            1 | 0xD => self.feature = v,
            2 => self.count = v,
            3 => self.sector = v,
            4 => self.cyl_lo = v,
            5 => self.cyl_hi = v,
            6 => self.head = v,
            7 if self.head & 0x10 == 0 => self.command(v),
            _ => {}
        }
    }

    fn devctl_write(&mut self, v: u8) {
        let was = self.devctl & 0x04 != 0;
        self.devctl = v & 0x06;
        if v & 0x04 != 0 {
            // SW Rst: 戻されるまでリセット中（BSY）（CF 6.1.5.9）
            self.status = ST_BSY;
            self.xfer = Xfer::None;
            self.intrq = false;
        } else if was {
            self.multiple = 0;
            let (_, h, s) = self.default_chs();
            self.cur_heads = h;
            self.cur_spt = s;
            self.ata_reset();
        }
    }

    // ---- コマンド ----

    /// タスクファイルのアドレス（LBA または CHS）を LBA にする。範囲外なら None。
    fn task_lba(&self) -> Option<u32> {
        let lba = if self.head & 0x40 != 0 {
            u32::from_le_bytes([self.sector, self.cyl_lo, self.cyl_hi, self.head & 0x0F])
        } else {
            let cyl = u16::from_le_bytes([self.cyl_lo, self.cyl_hi]) as u32;
            let head = (self.head & 0x0F) as u32;
            let sec = self.sector as u32;
            let (h, s) = (self.cur_heads as u32, self.cur_spt as u32);
            if sec == 0 || sec > s || head >= h || cyl >= self.cur_cyls() {
                return None;
            }
            (cyl * h + head) * s + sec - 1
        };
        (lba < self.sectors()).then_some(lba)
    }

    /// タスクファイルのアドレスを lba にする（完了時に最後のセクタを指す。CF 6.2.1.18）。
    fn set_task_lba(&mut self, lba: u32) {
        if self.head & 0x40 != 0 {
            let b = lba.to_le_bytes();
            self.sector = b[0];
            self.cyl_lo = b[1];
            self.cyl_hi = b[2];
            self.head = self.head & 0xF0 | b[3] & 0x0F;
        } else {
            let (h, s) = (self.cur_heads as u32, self.cur_spt as u32);
            let cyl = lba / (h * s);
            let rem = lba % (h * s);
            let [lo, hi, ..] = cyl.to_le_bytes();
            self.cyl_lo = lo;
            self.cyl_hi = hi;
            self.head = self.head & 0xF0 | (rem / s) as u8 & 0x0F;
            self.sector = (rem % s + 1) as u8;
        }
    }

    fn sector_count(&self) -> u32 {
        if self.count == 0 {
            256
        } else {
            self.count as u32
        }
    }

    fn finish_ok(&mut self) {
        self.status = ST_RDY | ST_DSC;
        self.xfer = Xfer::None;
        self.intrq = true;
    }

    fn abort(&mut self, err: u8) {
        self.error = err;
        self.status = ST_RDY | ST_DSC | ST_ERR;
        self.xfer = Xfer::None;
        self.intrq = true;
    }

    fn command(&mut self, cmd: u8) {
        self.error = 0;
        match cmd {
            // Identify Device（CF 6.2.1.6）: Read Sectors と同じ手順
            0xEC => {
                self.buf = self.identify();
                self.start_read(1, 1, false);
            }
            // Read Sector(s)（CF 6.2.1.18）・Read Multiple（6.2.1.17）
            0x20 | 0x21 | 0xC4 => {
                let block = if cmd == 0xC4 {
                    if self.multiple == 0 {
                        return self.abort(ER_ABRT);
                    }
                    self.multiple as u32
                } else {
                    1
                };
                let n = self.sector_count();
                match self.task_lba() {
                    Some(lba) if lba as u64 + n as u64 <= self.sectors() as u64 => {
                        self.lba = lba;
                        self.load_sector();
                        self.start_read(n, block, true);
                    }
                    _ => self.abort(ER_IDNF),
                }
            }
            // Write Sector(s)・w/o Erase・Write Verify（CF 6.2.1.36〜）、Write Multiple
            0x30 | 0x31 | 0x38 | 0x3C | 0xC5 | 0xCD => {
                let block = if cmd == 0xC5 || cmd == 0xCD {
                    if self.multiple == 0 {
                        return self.abort(ER_ABRT);
                    }
                    self.multiple as u32
                } else {
                    1
                };
                let n = self.sector_count();
                match self.task_lba() {
                    Some(lba) if lba as u64 + n as u64 <= self.sectors() as u64 => {
                        self.lba = lba;
                        self.start_write(n, block, true);
                    }
                    _ => self.abort(ER_IDNF),
                }
            }
            // Read Buffer・Write Buffer（CF 6.2.1.14・6.2.1.35）
            0xE4 => self.start_read(1, 1, false),
            0xE8 => self.start_write(1, 1, false),
            // Format Track（CF 6.2.1.5）: Write Sectors と同じ手順でデータは使わない
            // TODO: LBA のときのセクタ数・フォーマットの模様（FFh/00h）は未実装
            0x50 => self.start_write(1, 1, false),
            // Read Verify（6.2.1.19）・Seek（6.2.1.28）: 範囲の検査だけ
            0x40 | 0x41 | 0x70..=0x7F => match self.task_lba() {
                Some(_) => self.finish_ok(),
                None => self.abort(ER_IDNF),
            },
            // Erase Sector(s)（6.2.1.3）: 事前消去。中身は変えない
            0xC0 => match self.task_lba() {
                Some(_) => self.finish_ok(),
                None => self.abort(ER_IDNF),
            },
            // Set Multiple Mode（6.2.1.30）
            0xC6 => {
                if self.count == 0 {
                    self.multiple = 0;
                    self.finish_ok();
                } else if self.count <= MAX_MULTIPLE {
                    self.multiple = self.count;
                    self.finish_ok();
                } else {
                    self.multiple = 0;
                    self.abort(ER_ABRT);
                }
            }
            // Initialize Drive Parameters（6.2.1.9）
            0x91 => {
                self.cur_spt = self.count;
                self.cur_heads = (self.head & 0x0F) + 1;
                self.finish_ok();
            }
            // Execute Drive Diagnostic（6.2.1.2）: 01h = 異常なし
            0x90 => {
                self.finish_ok();
                self.error = 0x01;
            }
            // Check Power Mode（6.2.1.1）: 常にアイドル（FFh）
            0x98 | 0xE5 => {
                self.count = 0xFF;
                self.finish_ok();
            }
            // Recalibrate（1Xh）・Flush Cache・Idle・Standby・Sleep 系: 何もせず成功
            // Set Features（EFh）: TODO: 未対応の機能は中止すべき（CF 6.2.1.29）。
            // すべて受け付けている
            // Request Sense（03h）: 拡張エラー 00h（エラーなし）
            0x10..=0x1F | 0xE7 | 0xE0..=0xE3 | 0xE6 | 0x94..=0x97 | 0x99 | 0xEF | 0x03 => {
                self.finish_ok()
            }
            // NOP は常に中止（6.2.1.13）。Read/Write Long・DMA・Security・Key
            // Management・Translate Sector・Wear Level も未実装で中止。
            _ => self.abort(ER_ABRT),
        }
    }

    fn start_read(&mut self, left: u32, unit: u32, media: bool) {
        self.buf_pos = 0;
        self.xfer = Xfer::Read {
            left,
            block: unit.min(left),
            unit,
            media,
        };
        self.status = ST_RDY | ST_DSC | ST_DRQ;
        self.intrq = true;
        if media {
            self.set_task_lba(self.lba);
        }
    }

    fn start_write(&mut self, left: u32, unit: u32, media: bool) {
        self.buf_pos = 0;
        self.xfer = Xfer::Write {
            left,
            block: unit.min(left),
            unit,
            media,
        };
        // 最初のセクタの前は割り込みを出さない（CF 6.2.1.36 の手順）
        self.status = ST_RDY | ST_DSC | ST_DRQ;
        if media {
            self.set_task_lba(self.lba);
        }
    }

    fn load_sector(&mut self) {
        let o = self.lba as usize * SECTOR;
        self.buf.copy_from_slice(&self.disk[o..o + SECTOR]);
    }

    /// ホストが 1 セクタを読み終えた。
    fn read_sector_done(&mut self) {
        let Xfer::Read {
            left,
            block,
            unit,
            media,
        } = self.xfer
        else {
            return;
        };
        self.buf_pos = 0;
        if media {
            self.count = self.count.wrapping_sub(1);
        }
        let left = left - 1;
        if left == 0 {
            self.status = ST_RDY | ST_DSC;
            self.xfer = Xfer::None;
            return;
        }
        self.lba += 1;
        self.load_sector();
        self.set_task_lba(self.lba);
        let block = if block > 1 {
            block - 1
        } else {
            // 次のブロックの先頭: DRQ とともに割り込み（CF 6.2.1.17・6.2.1.18）
            self.intrq = true;
            unit.min(left)
        };
        self.xfer = Xfer::Read {
            left,
            block,
            unit,
            media,
        };
    }

    /// ホストが 1 セクタを書き終えた。
    fn write_sector_done(&mut self) {
        let Xfer::Write {
            left,
            block,
            unit,
            media,
        } = self.xfer
        else {
            return;
        };
        self.buf_pos = 0;
        if media {
            let o = self.lba as usize * SECTOR;
            self.disk[o..o + SECTOR].copy_from_slice(&self.buf);
            self.set_task_lba(self.lba);
            self.count = self.count.wrapping_sub(1);
        }
        let left = left - 1;
        if left == 0 {
            self.finish_ok();
            return;
        }
        self.lba += 1;
        let block = if block > 1 {
            block - 1
        } else {
            self.intrq = true;
            unit.min(left)
        };
        self.xfer = Xfer::Write {
            left,
            block,
            unit,
            media,
        };
    }

    /// IDENTIFY DEVICE の 256 ワード（CF Table 47）。
    fn identify(&self) -> Vec<u8> {
        let mut w = [0u16; 256];
        let (cyl, heads, spt) = self.default_chs();
        let total = self.sectors();
        w[0] = 0x848A; // CompactFlash の署名（6.2.1.6.1）
        w[1] = cyl;
        w[3] = heads as u16;
        w[6] = spt as u16;
        w[7] = (total >> 16) as u16; // word 7 が上位（Table 47）
        w[8] = total as u16;
        put_str(&mut w[10..20], SERIAL, true);
        w[22] = 0x0004;
        put_str(&mut w[23..27], FIRMWARE, false);
        put_str(&mut w[27..47], MODEL, false);
        w[47] = 0x8000 | MAX_MULTIPLE as u16;
        w[49] = 0x0200; // LBA あり・DMA なし（6.2.1.6.11）
        w[51] = 0x0200; // PIO モード 2（6.2.1.6.12）
        w[53] = 0x0001; // word 54〜58 が有効
        let cc = self.cur_cyls();
        w[54] = cc as u16;
        w[55] = self.cur_heads as u16;
        w[56] = self.cur_spt as u16;
        let cap = cc * self.cur_heads as u32 * self.cur_spt as u32;
        w[57] = cap as u16; // word 57 が下位（Table 47）
        w[58] = (cap >> 16) as u16;
        w[59] = 0x0100 | self.multiple as u16;
        // TODO: words 60〜61 の上下の順は CF に記載がない。ATA の慣習（60 が下位）
        w[60] = total as u16;
        w[61] = (total >> 16) as u16;
        let mut b = vec![0u8; SECTOR];
        for (i, v) in w.iter().enumerate() {
            b[2 * i..2 * i + 2].copy_from_slice(&v.to_le_bytes());
        }
        b
    }

    // ---- スナップショット ----

    /// カードの状態（ディスクの中身は save_disk で別に書く）。
    pub fn save_state(&self, e: &mut Encoder) {
        let CfCard {
            disk,
            powered,
            reset,
            cor,
            ccsr,
            prr,
            scr,
            error,
            feature,
            count,
            sector,
            cyl_lo,
            cyl_hi,
            head,
            status,
            devctl,
            intrq,
            buf,
            buf_pos,
            xfer,
            lba,
            multiple,
            cur_heads,
            cur_spt,
        } = self;
        e.u32((disk.len() / SECTOR) as u32);
        e.bool(*powered);
        e.bool(*reset);
        for v in [
            *cor, *ccsr, *prr, *scr, *error, *feature, *count, *sector, *cyl_lo, *cyl_hi, *head,
            *status, *devctl,
        ] {
            e.u8(v);
        }
        e.bool(*intrq);
        e.bytes(buf);
        e.u16(*buf_pos);
        xfer.encode(e);
        e.u32(*lba);
        e.u8(*multiple);
        e.u8(*cur_heads);
        e.u8(*cur_spt);
    }

    /// 状態を読む。ディスクは 0 で埋めた大きさだけ用意する（中身は load_disk_block）。
    pub fn load_state(d: &mut Decoder) -> Result<CfCard, Error> {
        let sectors = d.u32()?;
        if sectors == 0 || sectors > MAX_SECTORS {
            return d.err("bad card size");
        }
        let mut c = CfCard::new(vec![0; sectors as usize * SECTOR]).map_err(Error::Format)?;
        c.powered = d.bool()?;
        c.reset = d.bool()?;
        for f in [
            &mut c.cor,
            &mut c.ccsr,
            &mut c.prr,
            &mut c.scr,
            &mut c.error,
            &mut c.feature,
            &mut c.count,
            &mut c.sector,
            &mut c.cyl_lo,
            &mut c.cyl_hi,
            &mut c.head,
            &mut c.status,
            &mut c.devctl,
        ] {
            *f = d.u8()?;
        }
        c.intrq = d.bool()?;
        let buf = d.bytes()?;
        if buf.len() != SECTOR {
            return d.err("bad buffer size");
        }
        c.buf.copy_from_slice(buf);
        c.buf_pos = d.u16()?;
        if c.buf_pos as usize >= SECTOR {
            return d.err("bad buffer position");
        }
        c.xfer = Xfer::decode(d)?;
        c.lba = d.u32()?;
        c.multiple = d.u8()?;
        c.cur_heads = d.u8()?;
        c.cur_spt = d.u8()?;
        if c.lba >= sectors {
            return d.err("bad transfer address");
        }
        Ok(c)
    }

    /// ディスクを DISK_BLOCK バイトの区画に分け、0 でない区画だけを (番号, 中身) で返す
    /// （スナップショットでは空でない部分だけを保存する。2026-09-29 ユーザー決定）。
    pub fn disk_blocks(&self) -> impl Iterator<Item = (u32, &[u8])> {
        self.disk
            .chunks(DISK_BLOCK)
            .enumerate()
            .filter(|(_, b)| b.iter().any(|&x| x != 0))
            .map(|(i, b)| (i as u32, b))
    }

    /// disk_blocks で保存した区画を書き戻す。
    pub fn load_disk_block(&mut self, index: u32, data: &[u8]) -> Result<(), String> {
        let o = index as usize * DISK_BLOCK;
        let end = o.checked_add(data.len()).filter(|&e| e <= self.disk.len());
        match end {
            Some(end) if data.len() == DISK_BLOCK.min(self.disk.len() - o) => {
                self.disk[o..end].copy_from_slice(data);
                Ok(())
            }
            _ => Err(format!("bad disk block {index}")),
        }
    }
}

/// スナップショットの区画の大きさ（64KB）。
pub const DISK_BLOCK: usize = 64 << 10;

impl Card for CfCard {
    fn read(&mut self, space: Space, addr: u32, wide: bool) -> u16 {
        if !self.active() {
            return 0; // TODO: リセット中・電源なしのカードの応答は不定
        }
        if space == Space::Attr {
            // 属性メモリは偶数番地の 8 ビットだけが有効（CF 4.4.3。ワードの上位は無効）
            return self.attr_read(addr) as u16;
        }
        let Some(off) = self.taskfile_offset(space, addr) else {
            return 0;
        };
        if wide {
            // ワードアクセス: 0・8・9 はデータレジスタ（16 ビット）、他は偶数の
            // レジスタが下位・次のレジスタが上位（CF Table 44・4.5.1）
            if off == 0 || off == 8 || off == 9 {
                let lo = self.data_read_byte() as u16;
                let hi = self.data_read_byte() as u16;
                return lo | hi << 8;
            }
            let o = off & !1;
            let lo = self.reg_read(o) as u16;
            let hi = self.reg_read(o | 1) as u16;
            return lo | hi << 8;
        }
        self.reg_read(off) as u16
    }

    fn write(&mut self, space: Space, addr: u32, wide: bool, v: u16) {
        if !self.powered || self.reset {
            return;
        }
        if space == Space::Attr {
            self.attr_write(addr, v as u8);
            return;
        }
        if !self.active() {
            return;
        }
        let Some(off) = self.taskfile_offset(space, addr) else {
            return;
        };
        if wide {
            if off == 0 || off == 8 || off == 9 {
                self.data_write_byte(v as u8);
                self.data_write_byte((v >> 8) as u8);
                return;
            }
            let o = off & !1;
            self.reg_write(o, v as u8);
            self.reg_write(o | 1, (v >> 8) as u8);
            return;
        }
        self.reg_write(off, v as u8);
    }

    fn rdy_ireq(&self) -> bool {
        if self.conf() == 0 {
            // メモリのインタフェース: RDY（リセット中は Busy）
            self.active()
        } else {
            // I/O のインタフェース: -IREQ。TODO: パルスモード（COR の LevlREQ=0）は
            // レベルとして出している
            self.active() && self.irq_line()
        }
    }

    fn iois16(&self, _addr: u32) -> bool {
        true // CompactFlash は全 I/O アドレスで 16 ビットを許す（CF 4.5.1）
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

const SERIAL: &str = "CRLN00000001";
const FIRMWARE: &str = "1.0";
const MODEL: &str = "CErulean Virtual CF";

/// ATA の文字列（1 ワードに 2 文字、先の文字が上位バイト。空白で埋める）。
/// right なら右詰め（シリアル番号。CF 6.2.1.6.6）。
fn put_str(w: &mut [u16], s: &str, right: bool) {
    let n = w.len() * 2;
    let mut b = vec![b' '; n];
    let src = s.as_bytes();
    let len = src.len().min(n);
    let start = if right { n - len } else { 0 };
    b[start..start + len].copy_from_slice(&src[..len]);
    for (i, x) in w.iter_mut().enumerate() {
        *x = (b[2 * i] as u16) << 8 | b[2 * i + 1] as u16;
    }
}

/// CIS（属性メモリの偶数番地に 1 バイトずつ並ぶ）。書式は SD Table 6-1 の各
/// フィールドの説明に従う。製造者・製品は独自の値。
#[rustfmt::skip]
static CIS: &[u8] = &[
    // CISTPL_DEVICE: I/O 型のデバイス・WPS・拡張速度（SD 000h〜00Ah）
    0x01, 0x04, 0xDF, 0x12, 0x01, 0xFF,
    // CISTPL_MANFID: 製造者 0000h・製品 0000h（独自の値）
    0x20, 0x04, 0x00, 0x00, 0x00, 0x00,
    // CISTPL_VERS_1: 版 4.1、製造者・製品・版の文字列、終端 FFh（SD 02Ch〜05Ch）
    0x15, 0x1B, 0x04, 0x01,
    b'C', b'E', b'r', b'u', b'l', b'e', b'a', b'n', 0x00,
    b'V', b'i', b'r', b't', b'u', b'a', b'l', b' ', b'C', b'F', 0x00,
    b'1', b'.', b'0', 0x00,
    0xFF,
    // CISTPL_FUNCID: ディスク（04h）、POST でのインストール（SD 068h〜06Eh）
    0x21, 0x02, 0x04, 0x01,
    // CISTPL_FUNCE: インタフェースは PC Card-ATA（SD 070h〜076h）
    0x22, 0x02, 0x01, 0x01,
    // CISTPL_FUNCE: 基本の ATA の選択肢: シリコン・ID が一意・VPP 不要、
    // 省電力のモードとコマンド（SD 078h〜080h）
    0x22, 0x03, 0x02, 0x0C, 0x0F,
    // CISTPL_CONFIG: 大きさの欄 01h（基底 2 バイト・マスク 1 バイト）、最後の
    // 構成の番号 03h、構成レジスタは 200h、COR・CCSR・PRR・SCR あり（SD 082h〜08Eh）
    0x1A, 0x05, 0x01, 0x03, 0x00, 0x02, 0x0F,
    // CISTPL_CFTABLE_ENTRY 構成 0（既定・インタフェースの欄あり）: メモリのみ、
    // RDY/-BSY と WAIT を使う。VCC の電源（公称 5V・最小 4.5V・最大 5.5V・平均
    // 80mA）、2KB のメモリ空間、その他（省電力・ツイン）（SD 090h〜0A8h）
    0x1B, 0x0B, 0xC0, 0xC0, 0xA1, 0x27, 0x55, 0x4D, 0x5D, 0x75, 0x08, 0x00, 0x21,
    // CISTPL_CFTABLE_ENTRY 構成 1（既定）: I/O のインタフェース、RDY/-BSY を使う。
    // VCC の電源（同上）、I/O は 4 本のアドレス線（16 レジスタ）で 8/16 ビット、
    // 割り込みは共有・パルス・レベル・IRQ 0〜15 のマスク、その他（SD 0BAh〜0D6h）
    0x1B, 0x0D, 0xC1, 0x41, 0x99, 0x27, 0x55, 0x4D, 0x5D, 0x75, 0x64, 0xF0, 0xFF, 0xFF, 0x21,
    // CISTPL_CFTABLE_ENTRY 構成 2（既定）: AT の固定ディスクのプライマリ。I/O は
    // 10 本のアドレス線で範囲 2 個（1F0h〜1F7h・3F6h〜3F7h）、推奨 IRQ 14（SD 0E8h〜10Eh）。
    // atadisk.dll は I/O の範囲が 2 個の構成を探し、無ければメモリの構成を試す
    // （2026-09-29 観察）
    0x1B, 0x12, 0xC2, 0x41, 0x99, 0x27, 0x55, 0x4D, 0x5D, 0x75, 0xEA, 0x61,
    0xF0, 0x01, 0x07, 0xF6, 0x03, 0x01, 0xEE, 0x21,
    // CISTPL_CFTABLE_ENTRY 構成 3（既定）: セカンダリ（170h〜177h・376h〜377h）（SD 120h〜146h）
    0x1B, 0x12, 0xC3, 0x41, 0x99, 0x27, 0x55, 0x4D, 0x5D, 0x75, 0xEA, 0x61,
    0x70, 0x01, 0x07, 0x76, 0x03, 0x01, 0xEE, 0x21,
    // CISTPL_NO_LINK: 共通メモリの CIS を探させない（SD 164h）
    0x14, 0x00,
    // CISTPL_END
    0xFF,
];

#[cfg(test)]
mod tests {
    use super::*;

    /// CIS をタプルの鎖としてたどれ、各タプルのリンクが中身の長さと一致すること。
    #[test]
    fn cis_tuple_chain() {
        let mut i = 0;
        let mut codes = Vec::new();
        while CIS[i] != 0xFF {
            codes.push(CIS[i]);
            let link = CIS[i + 1] as usize;
            i += 2 + link;
            assert!(
                i < CIS.len(),
                "tuple {:02X} overruns the CIS",
                codes.last().unwrap()
            );
        }
        assert_eq!(i, CIS.len() - 1);
        assert_eq!(
            codes,
            [
                0x01, 0x20, 0x15, 0x21, 0x22, 0x22, 0x1A, 0x1B, 0x1B, 0x1B, 0x1B, 0x14
            ]
        );
    }

    fn card(sectors: usize) -> CfCard {
        let mut disk = vec![0u8; sectors * SECTOR];
        for (i, s) in disk.chunks_mut(SECTOR).enumerate() {
            s[0] = i as u8;
            s[511] = 0xA5;
        }
        let mut c = CfCard::new(disk).unwrap();
        c.set_power(true);
        c.set_reset(false);
        c
    }

    fn io_w(c: &mut CfCard, off: u32, v: u8) {
        c.write(Space::Io, 0x100 + off, false, v as u16);
    }
    fn io_r(c: &mut CfCard, off: u32) -> u8 {
        c.read(Space::Io, 0x100 + off, false) as u8
    }

    #[test]
    fn identify_and_read_write_in_io_mode() {
        let mut c = card(2048);
        // 構成 1（連続した 16 個の I/O レジスタ）・レベルモードの割り込み
        c.write(Space::Attr, 0x200, false, 0x41);
        assert_eq!(c.read(Space::Attr, 0x200, false), 0x41);
        io_w(&mut c, 6, 0xA0);
        io_w(&mut c, 7, 0xEC);
        assert!(c.rdy_ireq(), "IDENTIFY raises the interrupt");
        assert_eq!(io_r(&mut c, 7) & (ST_DRQ | ST_BSY), ST_DRQ);
        assert!(!c.rdy_ireq(), "reading status clears it");
        let mut id = [0u16; 256];
        for w in id.iter_mut() {
            *w = c.read(Space::Io, 0x100, true);
        }
        assert_eq!(id[0], 0x848A);
        assert_eq!((id[7] as u32) << 16 | id[8] as u32, 2048);
        assert_eq!(io_r(&mut c, 7) & ST_DRQ, 0);

        // LBA 5 から 2 セクタ読む
        for (o, v) in [(2, 2), (3, 5), (4, 0), (5, 0), (6, 0xE0)] {
            io_w(&mut c, o, v);
        }
        io_w(&mut c, 7, 0x20);
        let mut got = Vec::new();
        for _ in 0..2 {
            assert!(c.rdy_ireq());
            assert_eq!(io_r(&mut c, 7) & ST_DRQ, ST_DRQ);
            for _ in 0..256 {
                got.extend_from_slice(&c.read(Space::Io, 0x100, true).to_le_bytes());
            }
        }
        assert_eq!((got[0], got[511], got[512]), (5, 0xA5, 6));
        assert_eq!(io_r(&mut c, 7), ST_RDY | ST_DSC);
        assert_eq!(
            (io_r(&mut c, 2), io_r(&mut c, 3)),
            (0, 6),
            "points to the last sector"
        );

        // LBA 7 に 1 セクタ書く
        for (o, v) in [(2, 1), (3, 7), (6, 0xE0)] {
            io_w(&mut c, o, v);
        }
        io_w(&mut c, 7, 0x30);
        assert!(!c.rdy_ireq(), "no interrupt before the first sector");
        for i in 0..256u16 {
            c.write(Space::Io, 0x100, true, i);
        }
        assert!(c.rdy_ireq());
        assert_eq!(io_r(&mut c, 7), ST_RDY | ST_DSC);
        assert_eq!(&c.disk()[7 * SECTOR..7 * SECTOR + 4], &[0, 0, 1, 0]);
    }

    #[test]
    fn memory_mode_and_errors() {
        let mut c = card(64);
        // 構成 0（メモリ）: 共通メモリの 0〜F がタスクファイル
        c.write(Space::Common, 6, false, 0xE0);
        c.write(Space::Common, 3, false, 64); // 範囲外
        c.write(Space::Common, 2, false, 1);
        c.write(Space::Common, 7, false, 0x20);
        assert_eq!(
            c.read(Space::Common, 7, false) as u8,
            ST_RDY | ST_DSC | ST_ERR
        );
        assert_eq!(c.read(Space::Common, 1, false) as u8, ER_IDNF);
        c.write(Space::Common, 7, false, 0x00); // NOP は中止
        assert_eq!(c.read(Space::Common, 1, false) as u8, ER_ABRT);
        // SRESET で構成が戻る
        c.write(Space::Attr, 0x200, false, 0x81);
        c.write(Space::Attr, 0x200, false, 0x01);
        assert_eq!(c.read(Space::Attr, 0x200, false), 0);
    }

    #[test]
    fn disk_blocks_round_trip() {
        let c = card(300);
        let mut d = CfCard::new(vec![0; 300 * SECTOR]).unwrap();
        for (i, b) in c.disk_blocks() {
            d.load_disk_block(i, b).unwrap();
        }
        assert_eq!(c.disk(), d.disk());
        assert!(d.load_disk_block(99, &[0; 16]).is_err());
    }
}
