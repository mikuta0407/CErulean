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
            irq,
            fiq,
        } = self;
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

impl DmaStub {
    pub const STATE_VERSION: u16 = 1;

    pub fn save_state(&self, e: &mut Encoder) {
        let DmaStub { stub } = self;
        stub.save_state(e);
    }

    pub fn load_state(&mut self, d: &mut Decoder) -> Result<(), Error> {
        let DmaStub { stub } = self;
        stub.load_state(d)
    }
}
