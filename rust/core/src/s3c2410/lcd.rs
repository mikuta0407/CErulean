//! LCD コントローラ（データシート Ch.15）。

use std::fmt;

/// S3C2410 の LCD コントローラ。
///
/// 表示の生成（ピクセルクロック・同期信号）はエミュレートせず、レジスタ
/// から「フレームバッファがどこにあって、何ピクセル × 何ビットか」を
/// 解釈するだけにする。画面が必要になった時点で frame が RAM から画像を作る。
///
/// 対応範囲: TFT（PNRMODE=11）の 16bpp と 24bpp、および TFT 1/2/4/8bpp の
/// パレットモード。STN は未対応（Device Emulator 構成は TFT のはず）。
///
/// レジスタ（オフセット）:
///
/// ```text
/// 0x00 LCDCON1   LINECNT[27:18] CLKVAL[17:8] MMODE[7] PNRMODE[6:5] BPPMODE[4:1] ENVID[0]
/// 0x04 LCDCON2   VBPD[31:24] LINEVAL[23:14] VFPD[13:6] VSPW[5:0]
/// 0x08 LCDCON3   HBPD[25:19] HOZVAL[18:8] HFPD[7:0]
/// 0x0C LCDCON4   MVAL[15:8] HSPW[7:0]
/// 0x10 LCDCON5   BPP24BL[12] FRM565[11] ... BSWP[1] HWSWP[0]
/// 0x14 LCDSADDR1 LCDBANK[29:21]=A[30:22] LCDBASEU[20:0]=A[21:1]
/// 0x18 LCDSADDR2 LCDBASEL[20:0]=A[21:1]（フレーム終端）
/// 0x1C LCDSADDR3 OFFSIZE[21:11] PAGEWIDTH[10:0]（いずれもハーフワード単位）
/// 0x400〜0x7FC   パレット（256 エントリ）
/// ```
///
/// 上記ビット配置と、16bpp で HWSWP=1 なら下位ハーフワードが左ピクセル
/// （Figure 15-5）であることは User's Manual Rev 1.1 の Ch.15 で確認済み。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lcd {
    /// 0x00〜0x6C
    pub(crate) regs: [u32; 0x70 / 4],
    pub(crate) palette: Box<[u32; 256]>,
}

const REG_LCDCON1: u32 = 0x00;
const REG_LCDCON2: u32 = 0x04;
const REG_LCDCON3: u32 = 0x08;
const REG_LCDCON5: u32 = 0x10;
const REG_LCDSADDR1: u32 = 0x14;
const REG_LCDSADDR3: u32 = 0x1C;
const LCD_PAL_BASE: u32 = 0x400;
const LCD_PAL_END: u32 = 0x800;

/// レジスタから解釈した表示設定。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LcdConfig {
    /// ENVID（映像出力）
    pub enabled: bool,
    /// PNRMODE == 11
    pub tft: bool,
    /// 1/2/4/8/16/24（未対応モードは 0）
    pub bpp: u32,
    /// HOZVAL+1
    pub width: u32,
    /// LINEVAL+1
    pub height: u32,
    /// フレームバッファ先頭の物理アドレス
    pub base: u32,
    /// 1 行のバイト数（PAGEWIDTH+OFFSIZE、ハーフワード単位 ×2）
    pub stride: u32,
    /// HWSWP: 16bpp でワード内のハーフワード順を入れ替える
    pub hw_swap: bool,
    /// BSWP: ワード内のバイト順を入れ替える
    pub byte_swap: bool,
    /// BPP24BL: 24bpp で有効データが下位 24 ビット側か
    pub bpp24_low: bool,
    /// FRM565: 16bpp を 5:6:5 として扱う（0 なら 5:5:5:I）
    pub frm565: bool,
    /// ハーフワード単位（診断用）
    pub page_width: u32,
    /// ハーフワード単位（診断用）
    pub off_size: u32,
}

impl fmt::Display for LcdConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "enabled={} tft={} {}x{} {}bpp base={:08X} stride={} hwswp={} bswp={} frm565={}",
            self.enabled,
            self.tft,
            self.width,
            self.height,
            self.bpp,
            self.base,
            self.stride,
            self.hw_swap,
            self.byte_swap,
            self.frm565
        )
    }
}

/// 画面（RGBA、左上から行順、1 画素 R,G,B,A の 4 バイト、行の詰め物なし）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// 画面を作れなかった理由。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FrameError {
    /// 表示が未対応のモード（STN・BPPMODE の予約値など）
    Unsupported(LcdConfig),
    /// フレームバッファの読み出しがバスエラー
    Bus(crate::bus::BusError),
}

impl fmt::Display for FrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FrameError::Unsupported(c) => write!(f, "lcd: unsupported mode ({c})"),
            FrameError::Bus(b) => b.fmt(f),
        }
    }
}

impl Default for Lcd {
    fn default() -> Self {
        Self::new()
    }
}

impl Lcd {
    pub fn new() -> Lcd {
        Lcd {
            regs: [0; 0x70 / 4],
            palette: Box::new([0; 256]),
        }
    }

    pub fn read(&self, off: u32, _size: u32) -> u32 {
        let off = off & !3;
        if (LCD_PAL_BASE..LCD_PAL_END).contains(&off) {
            return self.palette[((off - LCD_PAL_BASE) / 4) as usize];
        }
        // LINECNT（LCDCON1[27:18]）と VSTATUS/HSTATUS（LCDCON5[16:13]）は
        // 走査位置を示す読み出し専用フィールド。走査はエミュレートしないので 0
        // （= 1 行目・VSYNC 期間）を返す。
        // TODO: ドライバが垂直同期待ちでこれをポーリングしてハングしたら、
        // 仮想時間から走査位置を生成する。
        self.regs.get((off / 4) as usize).copied().unwrap_or(0)
    }

    pub fn write(&mut self, off: u32, _size: u32, v: u32) {
        let off = off & !3;
        if (LCD_PAL_BASE..LCD_PAL_END).contains(&off) {
            self.palette[((off - LCD_PAL_BASE) / 4) as usize] = v;
            return;
        }
        let v = match off {
            REG_LCDCON1 => v & !(0x3FF << 18), // LINECNT は読み出し専用
            REG_LCDCON5 => v & !(0xF << 13),   // VSTATUS/HSTATUS は読み出し専用
            _ => v,
        };
        if let Some(r) = self.regs.get_mut((off / 4) as usize) {
            *r = v;
        }
    }

    /// 現在のレジスタ値を解釈する。
    pub fn config(&self) -> LcdConfig {
        let r = |o: u32| self.regs[(o / 4) as usize];
        let (con1, con2, con3, con5) = (
            r(REG_LCDCON1),
            r(REG_LCDCON2),
            r(REG_LCDCON3),
            r(REG_LCDCON5),
        );
        let (sa1, sa3) = (r(REG_LCDSADDR1), r(REG_LCDSADDR3));
        let mut c = LcdConfig {
            enabled: con1 & 1 != 0,
            tft: (con1 >> 5) & 3 == 3,
            width: ((con3 >> 8) & 0x7FF) + 1,
            height: ((con2 >> 14) & 0x3FF) + 1,
            hw_swap: con5 & (1 << 0) != 0,
            byte_swap: con5 & (1 << 1) != 0,
            frm565: con5 & (1 << 11) != 0,
            bpp24_low: con5 & (1 << 12) != 0,
            page_width: sa3 & 0x7FF,
            off_size: (sa3 >> 11) & 0x7FF,
            ..Default::default()
        };
        if c.tft {
            // TFT の BPPMODE: 1000=1, 1001=2, 1010=4, 1011=8, 1100=16, 1101=24bpp。
            c.bpp = match (con1 >> 1) & 0xF {
                0x8 => 1,
                0x9 => 2,
                0xA => 4,
                0xB => 8,
                0xC => 16,
                0xD => 24,
                _ => 0,
            };
        }
        // フレームバッファ先頭: LCDBANK が A[30:22]、LCDBASEU が A[21:1]。
        c.base = ((sa1 >> 21) & 0x1FF) << 22 | (sa1 & 0x1FFFFF) << 1;
        c.stride = (c.page_width + c.off_size) * 2;
        c
    }

    /// フレームバッファを RGBA 画像に変換する。read32 は物理アドレスの
    /// ワード読み出し（machine がバスを渡す）。表示無効・未対応モードなら Err。
    /// Go と同じく画素ごとにワードを読む（監視の記録も Go と同じになる）。
    pub fn frame(
        &self,
        mut read32: impl FnMut(u32) -> Result<u32, crate::bus::BusError>,
    ) -> Result<(Frame, LcdConfig), FrameError> {
        let c = self.config();
        if !c.tft || c.bpp == 0 {
            return Err(FrameError::Unsupported(c));
        }
        let mut rgba = Vec::with_capacity((c.width * c.height * 4) as usize);
        for y in 0..c.height {
            let row = c.base.wrapping_add(y.wrapping_mul(c.stride));
            for x in 0..c.width {
                let px = self
                    .pixel(&c, row, x, &mut read32)
                    .map_err(FrameError::Bus)?;
                rgba.extend_from_slice(&px);
            }
        }
        Ok((
            Frame {
                width: c.width,
                height: c.height,
                rgba,
            },
            c,
        ))
    }

    /// 行先頭 row から x 番目のピクセル。
    ///
    /// メモリ上のピクセル順（データシートの「メモリデータフォーマット」表）:
    /// スワップなしでは 1 ワード内の最上位側が画面左のピクセルになる
    /// （ビッグエンディアン的な並び）。WinCE（リトルエンディアン）は 16bpp で
    /// HWSWP=1 にして、下位ハーフワードを左ピクセルにする想定。
    fn pixel(
        &self,
        c: &LcdConfig,
        row: u32,
        x: u32,
        read32: &mut impl FnMut(u32) -> Result<u32, crate::bus::BusError>,
    ) -> Result<[u8; 4], crate::bus::BusError> {
        let mut word = |pa: u32| -> Result<u32, crate::bus::BusError> {
            let w = read32(pa & !3)?;
            // BSWP: ワード内のバイト順を入れ替える
            Ok(if c.byte_swap { w.swap_bytes() } else { w })
        };
        match c.bpp {
            24 => {
                // 24bpp は 1 ピクセル 1 ワード。BPP24BL=0 なら [23:0] が RGB888、
                // 1 なら [31:8]。
                let mut w = word(row.wrapping_add(x.wrapping_mul(4)))?;
                if c.bpp24_low {
                    w >>= 8;
                }
                Ok([(w >> 16) as u8, (w >> 8) as u8, w as u8, 0xFF])
            }
            16 => {
                let w = word(row.wrapping_add(x.wrapping_mul(2)))?;
                // スワップなし: 左ピクセル（偶数 x）が D[31:16]。HWSWP=1 なら D[15:0]。
                let first = x & 1 == 0;
                let p = if first != c.hw_swap {
                    w >> 16
                } else {
                    w & 0xFFFF
                };
                Ok(rgb565(p, c.frm565))
            }
            _ => {
                // パレットモード: 1 ワードに 32/BPP 個、最上位側が左ピクセル。
                let per_word = 32 / c.bpp;
                let w = word(row.wrapping_add((x / per_word).wrapping_mul(4)))?;
                let shift = 32 - c.bpp * (x % per_word + 1);
                let idx = (w >> shift) & ((1 << c.bpp) - 1);
                // TODO: パレットエントリの形式は TPAL/LCDCON5 FRM565 に従う
                // （5:6:5 か 5:5:5:I）。当面 16bpp と同じ解釈をする。
                Ok(rgb565(self.palette[idx as usize] & 0xFFFF, c.frm565))
            }
        }
    }
}

/// 16 ビット画素を 8 ビット RGB に展開する（RGBA、A=0xFF）。
/// FRM565=0 の 5:5:5:I 形式は R[15:11] G[10:6] B[5:1] I[0]（I は各色の
/// 最下位ビット）。下位ビットは上位ビットの複製で埋めて 0〜255 に広げる。
pub(crate) fn rgb565(p: u32, is565: bool) -> [u8; 4] {
    let r5 = (p >> 11) & 0x1F;
    if is565 {
        let g6 = (p >> 5) & 0x3F;
        let b5 = p & 0x1F;
        return [
            (r5 << 3 | r5 >> 2) as u8,
            (g6 << 2 | g6 >> 4) as u8,
            (b5 << 3 | b5 >> 2) as u8,
            0xFF,
        ];
    }
    // 5:5:5:I: G は [10:6]、B は [5:1]、I を各色の最下位として付ける。
    let i = p & 1;
    let g5 = (p >> 6) & 0x1F;
    let b5 = (p >> 1) & 0x1F;
    let (r6, g6, b6) = (r5 << 1 | i, g5 << 1 | i, b5 << 1 | i);
    [
        (r6 << 2 | r6 >> 4) as u8,
        (g6 << 2 | g6 >> 4) as u8,
        (b6 << 2 | b6 >> 4) as u8,
        0xFF,
    ]
}
