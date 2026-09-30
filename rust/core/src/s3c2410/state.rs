//! 周辺機器の状態の保存と読み込み（snapshot の部品）。
//!
//! 保存では構造体を `..` なしで全フィールド分解する（フィールドを足して保存を
//! 忘れるとコンパイルエラーになる）。構成で決まるもの（RTC の pclk_hz、UART の
//! 出力先の有無、スタブの forced）と派生情報（UART の未取り出しの送信バイト）は
//! `_` と明示して保存しない。

use crate::snapshot::{Decoder, Encoder, Error};

use super::*;

impl Intc {
    pub const STATE_VERSION: u16 = 1;

    pub fn save_state(&self, e: &mut Encoder) {
        let Intc {
            srcpnd,
            intmod,
            intmsk,
            priority,
            intpnd,
            intoffset,
            subsrcpnd,
            intsubmsk,
            level_src: _,
            irq,
            fiq,
        } = self;
        e.u32s(&[
            *srcpnd, *intmod, *intmsk, *priority, *intpnd, *intoffset, *subsrcpnd, *intsubmsk,
        ]);
        e.bool(*irq);
        e.bool(*fiq);
    }

    pub fn load_state(&mut self, d: &mut Decoder) -> Result<(), Error> {
        let Intc {
            srcpnd,
            intmod,
            intmsk,
            priority,
            intpnd,
            intoffset,
            subsrcpnd,
            intsubmsk,
            level_src,
            irq,
            fiq,
        } = self;
        *level_src = 0;
        [
            *srcpnd, *intmod, *intmsk, *priority, *intpnd, *intoffset, *subsrcpnd, *intsubmsk,
        ] = d.u32s()?;
        *irq = d.bool()?;
        *fiq = d.bool()?;
        Ok(())
    }
}

impl PwmTimer {
    pub const STATE_VERSION: u16 = 1;

    pub fn save_state(&self, e: &mut Encoder) {
        let PwmTimer {
            tcfg0,
            tcfg1,
            tcon,
            tcntb,
            tcmpb,
            cnt,
            running,
        } = self;
        e.u32s(&[*tcfg0, *tcfg1, *tcon]);
        e.u32s(tcntb);
        e.u32s(tcmpb);
        for (&c, &r) in cnt.iter().zip(running) {
            e.i64(c);
            e.bool(r);
        }
    }

    pub fn load_state(&mut self, d: &mut Decoder) -> Result<(), Error> {
        let PwmTimer {
            tcfg0,
            tcfg1,
            tcon,
            tcntb,
            tcmpb,
            cnt,
            running,
        } = self;
        [*tcfg0, *tcfg1, *tcon] = d.u32s()?;
        *tcntb = d.u32s()?;
        *tcmpb = d.u32s()?;
        for (c, r) in cnt.iter_mut().zip(running.iter_mut()) {
            *c = d.i64()?;
            *r = d.bool()?;
        }
        Ok(())
    }
}

impl Lcd {
    pub const STATE_VERSION: u16 = 1;

    pub fn save_state(&self, e: &mut Encoder) {
        let Lcd { regs, palette } = self;
        e.u32s(regs);
        e.u32s(&palette[..]);
    }

    pub fn load_state(&mut self, d: &mut Decoder) -> Result<(), Error> {
        let Lcd { regs, palette } = self;
        *regs = d.u32s()?;
        **palette = d.u32s()?;
        Ok(())
    }
}

impl Stub {
    pub const STATE_VERSION: u16 = 1;

    /// 保持値をオフセット順に書く（BTreeMap なので反復順が決まっている）。
    pub fn save_state(&self, e: &mut Encoder) {
        let Stub { regs, forced: _ } = self;
        e.u64(regs.len() as u64);
        for (&off, &v) in regs {
            e.u32(off);
            e.u32(v);
        }
    }

    pub fn load_state(&mut self, d: &mut Decoder) -> Result<(), Error> {
        let Stub { regs, forced: _ } = self;
        let n = d.count(0x10000)?;
        regs.clear();
        for _ in 0..n {
            let off = d.u32()?;
            let v = d.u32()?;
            if off & 3 != 0 || regs.insert(off, v).is_some() {
                return d.err(format!("bad register offset {off:X}"));
            }
        }
        Ok(())
    }
}

impl Rtc {
    pub const STATE_VERSION: u16 = 1;

    pub fn save_state(&self, e: &mut Encoder) {
        let Rtc {
            base,
            elapsed,
            pclk_hz: _,
            rtccon,
            other,
        } = self;
        e.i64(*base);
        e.i64(*elapsed);
        e.u32(*rtccon);
        other.save_state(e);
    }

    pub fn load_state(&mut self, d: &mut Decoder) -> Result<(), Error> {
        let Rtc {
            base,
            elapsed,
            pclk_hz: _,
            rtccon,
            other,
        } = self;
        *base = d.i64()?;
        *elapsed = d.i64()?;
        if *elapsed < 0 {
            return d.err("negative elapsed ticks");
        }
        *rtccon = d.u32()?;
        other.load_state(d)
    }
}

impl Adc {
    pub const STATE_VERSION: u16 = 1;

    pub fn save_state(&self, e: &mut Encoder) {
        let Adc {
            adccon,
            adctsc,
            adcdly,
            dat0,
            dat1,
            converting,
            ecflg,
            pen_down,
            raw_x,
            raw_y,
        } = self;
        e.u32s(&[*adccon, *adctsc, *adcdly, *dat0, *dat1]);
        e.i64(*converting);
        e.bool(*ecflg);
        e.bool(*pen_down);
        e.u32s(&[*raw_x, *raw_y]);
    }

    pub fn load_state(&mut self, d: &mut Decoder) -> Result<(), Error> {
        let Adc {
            adccon,
            adctsc,
            adcdly,
            dat0,
            dat1,
            converting,
            ecflg,
            pen_down,
            raw_x,
            raw_y,
        } = self;
        [*adccon, *adctsc, *adcdly, *dat0, *dat1] = d.u32s()?;
        *converting = d.i64()?;
        if *converting < 0 {
            return d.err("negative conversion time");
        }
        *ecflg = d.bool()?;
        *pen_down = d.bool()?;
        [*raw_x, *raw_y] = d.u32s()?;
        Ok(())
    }
}

impl Spi {
    pub const STATE_VERSION: u16 = 1;

    pub fn save_state(&self, e: &mut Encoder) {
        let Spi { ch } = self;
        for c in ch {
            let spi::SpiChannel {
                spcon,
                sppin,
                sppre,
                rx,
            } = c;
            e.u32s(&[*spcon, *sppin, *sppre, *rx]);
        }
    }

    pub fn load_state(&mut self, d: &mut Decoder) -> Result<(), Error> {
        let Spi { ch } = self;
        for c in ch.iter_mut() {
            let spi::SpiChannel {
                spcon,
                sppin,
                sppre,
                rx,
            } = c;
            [*spcon, *sppin, *sppre, *rx] = d.u32s()?;
        }
        Ok(())
    }
}

impl Uart {
    pub const STATE_VERSION: u16 = 1;

    /// 送信済みでまだ取り出されていないバイト（tx）は出力側の都合なので保存しない
    /// （保存の前に呼び出し側が取り出す）。
    pub fn save_state(&self, e: &mut Encoder) {
        let Uart {
            ulcon,
            ucon,
            ufcon,
            umcon,
            ubrdiv,
            capture: _,
            tx: _,
        } = self;
        e.u32s(&[*ulcon, *ucon, *ufcon, *umcon, *ubrdiv]);
    }

    pub fn load_state(&mut self, d: &mut Decoder) -> Result<(), Error> {
        let Uart {
            ulcon,
            ucon,
            ufcon,
            umcon,
            ubrdiv,
            capture: _,
            tx,
        } = self;
        [*ulcon, *ucon, *ufcon, *umcon, *ubrdiv] = d.u32s()?;
        tx.clear();
        Ok(())
    }
}

impl Dma {
    /// 2: チャネルの状態（転送中のカウンタ・アドレス・未読の区切り）を持つ実装に
    /// した（2026-09-30）。1 は値保持スタブのレジスタ（[`load_state_v1`](Self::load_state_v1)）。
    pub const STATE_VERSION: u16 = 2;

    /// 区切りを迎えて RAM から読んでいない転送（ready）は、保存の前にボードが
    /// 取り出す（flush_audio）。
    pub fn save_state(&self, e: &mut Encoder) {
        let Dma { ch, ready } = self;
        debug_assert!(ready.is_empty());
        for c in ch {
            let dma::DmaChannel {
                disrc,
                disrcc,
                didst,
                didstc,
                dcon,
                on,
                curr_tc,
                curr_src,
                curr_dst,
                seg_src,
                seg_units,
            } = c;
            e.u32s(&[
                *disrc, *disrcc, *didst, *didstc, *dcon, *curr_tc, *curr_src, *curr_dst, *seg_src,
                *seg_units,
            ]);
            e.bool(*on);
        }
    }

    pub fn load_state(&mut self, d: &mut Decoder) -> Result<(), Error> {
        let Dma { ch, ready } = self;
        ready.clear();
        for c in ch.iter_mut() {
            let dma::DmaChannel {
                disrc,
                disrcc,
                didst,
                didstc,
                dcon,
                on,
                curr_tc,
                curr_src,
                curr_dst,
                seg_src,
                seg_units,
            } = c;
            [
                *disrc, *disrcc, *didst, *didstc, *dcon, *curr_tc, *curr_src, *curr_dst, *seg_src,
                *seg_units,
            ] = d.u32s()?;
            *on = d.bool()?;
            if *curr_tc > 0xFFFFF {
                return d.err("dma: CURR_TC out of range");
            }
        }
        Ok(())
    }

    /// 版数 1（値保持スタブ）から: 書かれたレジスタだけを引き継ぎ、転送は始まって
    /// いない（CURR_TC=0）ものとする。ON_OFF は書かれた値のまま（旧版で止まっていた
    /// 再生は、読み込み後に DMA の要求で進み出す）。
    pub fn load_state_v1(&mut self, d: &mut Decoder) -> Result<(), Error> {
        let mut st = Stub::new(&[]);
        st.load_state(d)?;
        *self = Dma::new();
        for (n, c) in self.ch.iter_mut().enumerate() {
            let r = |o: u32| st.read(n as u32 * 0x40 + o, 4);
            c.disrc = r(0x00) & 0x7FFF_FFFF;
            c.disrcc = r(0x04) & 3;
            c.didst = r(0x08) & 0x7FFF_FFFF;
            c.didstc = r(0x0C) & 3;
            c.dcon = r(0x10);
            c.on = r(0x20) & 2 != 0;
        }
        Ok(())
    }
}

impl Iis {
    pub const STATE_VERSION: u16 = 1;

    pub fn save_state(&self, e: &mut Encoder) {
        let Iis {
            con,
            mode,
            psr,
            fcon,
            tx_count,
            phase,
            right,
        } = self;
        e.u32s(&[*con, *mode, *psr, *fcon, *tx_count]);
        e.i64(*phase);
        e.bool(*right);
    }

    pub fn load_state(&mut self, d: &mut Decoder) -> Result<(), Error> {
        let Iis {
            con,
            mode,
            psr,
            fcon,
            tx_count,
            phase,
            right,
        } = self;
        [*con, *mode, *psr, *fcon, *tx_count] = d.u32s()?;
        *phase = d.i64()?;
        *right = d.bool()?;
        if *tx_count > 32 || *phase < 0 || (self.tx_running() && self.phase == 0) {
            return d.err("iis: bad FIFO count or shift time");
        }
        Ok(())
    }

    /// machine の版数 4 まで（値保持スタブ）から: 書かれたレジスタだけを引き継ぐ
    /// （FIFO は空。送信中なら次の送り出しは 1 間隔後）。
    pub fn load_state_stub(&mut self, d: &mut Decoder) -> Result<(), Error> {
        let mut st = Stub::new(&[]);
        st.load_state(d)?;
        *self = Iis::new();
        self.con = st.read(0x00, 4) & 0x3F;
        self.mode = st.read(0x04, 4) & 0x1FF;
        self.psr = st.read(0x08, 4) & 0x3FF;
        self.fcon = st.read(0x0C, 4) & 0xF000;
        self.phase = self.half_ticks();
        Ok(())
    }
}
