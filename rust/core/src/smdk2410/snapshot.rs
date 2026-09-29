//! マシン全体のスナップショット（チャンクの並び。形式は crate::snapshot）。
//!
//! チャンクの並び（順序固定）:
//!
//! ```text
//! machine      命令数・仮想時間の端数・エントリ
//! cpu          レジスタ・バンク・PSR
//! mmu          CP15 とソフト TLB
//! ram          SDRAM の中身（128MB）
//! kbd          SPI1 のキーボード用マイコン（バス外のボード部品）
//! intc timer lcd rtc adc spi uart0 uart1 uart2 dma   周辺機器
//! stub:<名前>  値保持スタブ（board.rs の StubId の順）
//! pcic         PC カードコントローラ（machine の版数 2 から）
//! cf           ソケットの CompactFlash の状態（カードが挿さっているときだけ）
//! cf:blk       カードのディスクの 64KB の区画（番号 u32 と中身。0 でない区画だけ）
//! ```
//!
//! machine の版数 1（PC カードの前）は読み込める: コントローラは初期状態・カードなし、
//! GPIO の EINTPEND は 0 にする（版数 1 では値保持スタブで、意味のある値ではない）。
//!
//! 派生情報（デコードキャッシュ・世代・watched・命令履歴・実行ループの作業領域・
//! アイドルスキップの観測）は含めない。時間の溜め（pending）は保存前に必ず
//! デバイスへ同期する。

use std::io::{Read, Write};

use crate::snapshot::{self, Encoder, Error, Reader, Writer, format_err};

use super::board::{NUM_STUBS, StubId};
use crate::pccard::{CfCard, Pd6710};
use crate::s3c2410::eint;

/// machine チャンクの版数（2: PC カードのチャンクを足した。2026-09-29）。
const MACHINE_VERSION: u16 = 2;
use super::{Machine, SDRAM_BASE, SDRAM_SIZE};

const STUB_NAMES: [&str; NUM_STUBS] = [
    "memc",
    "usbhost",
    "clkpwr",
    "nand",
    "wdt",
    "iic",
    "usbdev",
    "sdi",
    "gpio",
    "iis",
    "de-paravirt",
];

impl Machine {
    /// 全状態を w に書く（命令境界で呼ぶこと。run_until の外）。image_id は元
    /// イメージの識別子（イメージファイル全体の SHA-256。照合は呼び出し側）。
    /// UART の送信バイトは保存しないので、先に take_uart1 で取り出しておくこと。
    pub fn save_snapshot(&mut self, w: impl Write, image_id: &str) -> Result<(), Error> {
        // 溜めたティックをデバイスに渡す（同期は状態の見え方を変えない）。
        self.sys.board.sync_time();
        let mut s = Writer::new(w, self.name(), image_id)?;
        let Machine {
            cpu,
            sys,
            entry_pa,
            idle_skip: _,
            poll: _,
            skipped: _,
        } = self;
        let super::Sys {
            mmu,
            bus,
            board,
            code: _,
            jit: _,
        } = sys;
        let super::Board {
            intc,
            timer,
            lcd,
            rtc,
            adc,
            spi,
            kbd,
            pcic,
            card,
            eint_levels: _,
            uart,
            dma,
            stubs,
            tick_acc,
            steps,
            pending,
            deadline: _,
            in_run,
            accounted: _,
            run: _,
        } = board;
        debug_assert!(*pending == 0 && !*in_run);
        s.chunk("machine", MACHINE_VERSION, |e| {
            e.u64(*steps);
            e.u32(*tick_acc);
            e.u32(*entry_pa);
        })?;
        s.chunk("cpu", crate::arm::Cpu::STATE_VERSION, |e| cpu.save_state(e))?;
        s.chunk("mmu", crate::mmu::Mmu::STATE_VERSION, |e| mmu.save_state(e))?;
        let (ram, _) = bus.ram(SDRAM_BASE).expect("SDRAM is mapped");
        s.raw_chunk("ram", 1, ram)?;
        s.chunk("kbd", 1, |e| {
            let super::KbdMcu { out, log: _ } = kbd;
            e.bytes(out.make_contiguous());
        })?;
        s.chunk("intc", 1, |e| intc.save_state(e))?;
        s.chunk("timer", 1, |e| timer.save_state(e))?;
        s.chunk("lcd", 1, |e| lcd.save_state(e))?;
        s.chunk("rtc", 1, |e| rtc.save_state(e))?;
        s.chunk("adc", 1, |e| adc.save_state(e))?;
        s.chunk("spi", 1, |e| spi.save_state(e))?;
        for (i, u) in uart.iter().enumerate() {
            s.chunk(&format!("uart{i}"), 1, |e| u.save_state(e))?;
        }
        s.chunk("dma", 1, |e| dma.save_state(e))?;
        for (name, st) in STUB_NAMES.iter().zip(stubs.iter()) {
            s.chunk(&format!("stub:{name}"), 1, |e: &mut Encoder| {
                st.save_state(e)
            })?;
        }
        s.chunk("pcic", Pd6710::STATE_VERSION, |e| pcic.save_state(e))?;
        if let Some(c) = card {
            s.chunk("cf", CfCard::STATE_VERSION, |e| c.save_state(e))?;
            for (i, b) in c.disk_blocks() {
                s.chunk("cf:blk", 1, |e| {
                    e.u32(i);
                    e.bytes(b);
                })?;
            }
        }
        s.finish()?;
        Ok(())
    }

    /// r から全状態を読み込む（Machine::new の直後に呼ぶ。監視は先に登録しておく）。
    /// 戻り値は保存時のイメージ ID。壊れた入力でも panic せずエラーを返す。
    pub fn load_snapshot(&mut self, r: impl Read) -> Result<String, Error> {
        let mut s = Reader::new(r)?;
        if s.header.machine != self.name() {
            return format_err(format!(
                "snapshot is for machine {:?}, not {}",
                s.header.machine,
                self.name()
            ));
        }
        let c = s.expect("machine")?;
        let version = c.version;
        if version != 1 && version != MACHINE_VERSION {
            return format_err(format!("machine: unsupported version {version}"));
        }
        let mut d = c.decoder(version)?;
        let (steps, tick_acc, entry_pa) = (d.u64()?, d.u32()?, d.u32()?);
        d.finish()?;
        if tick_acc >= 8 {
            return format_err("machine: bad tick fraction");
        }
        let c = s.expect("cpu")?;
        let mut d = c.decoder(crate::arm::Cpu::STATE_VERSION)?;
        self.cpu.load_state(&mut d)?;
        d.finish()?;
        let mmu_chunk = s.expect("mmu")?; // RAM の後で読む（TLB の RAM の位置は構成から決まる）
        let (ram, _) = self.sys.bus.ram_mut(SDRAM_BASE).expect("SDRAM is mapped");
        debug_assert_eq!(ram.len(), SDRAM_SIZE as usize);
        if s.expect_raw_into("ram", ram)? != 1 {
            return format_err("ram: bad version");
        }
        {
            let super::Sys {
                mmu, bus, board, ..
            } = &mut self.sys;
            let phys = crate::bus::BusPhys { bus, devs: board };
            let mut d = mmu_chunk.decoder(crate::mmu::Mmu::STATE_VERSION)?;
            mmu.load_state(&mut d, &phys)?;
            d.finish()?;
        }
        let b = &mut self.sys.board;
        let c = s.expect("kbd")?;
        let mut d = c.decoder(1)?;
        let out = d.bytes()?;
        if out.len() > 1 << 20 {
            return format_err("kbd: queue too long");
        }
        b.kbd.out = out.iter().copied().collect();
        b.kbd.log.clear();
        d.finish()?;
        macro_rules! dev {
            ($name:expr, $f:expr) => {{
                let c = s.expect($name)?;
                let mut d = c.decoder(1)?;
                $f(&mut d)?;
                d.finish()?;
            }};
        }
        dev!("intc", |d: &mut snapshot::Decoder| b.intc.load_state(d));
        dev!("timer", |d: &mut snapshot::Decoder| b.timer.load_state(d));
        dev!("lcd", |d: &mut snapshot::Decoder| b.lcd.load_state(d));
        dev!("rtc", |d: &mut snapshot::Decoder| b.rtc.load_state(d));
        dev!("adc", |d: &mut snapshot::Decoder| b.adc.load_state(d));
        dev!("spi", |d: &mut snapshot::Decoder| b.spi.load_state(d));
        for i in 0..3 {
            dev!(&format!("uart{i}"), |d: &mut snapshot::Decoder| b.uart[i]
                .load_state(d));
        }
        dev!("dma", |d: &mut snapshot::Decoder| b.dma.load_state(d));
        for (i, name) in STUB_NAMES.iter().enumerate() {
            dev!(&format!("stub:{name}"), |d: &mut snapshot::Decoder| b.stubs
                [i]
                .load_state(d));
        }
        let mut pcic = Pd6710::new();
        let mut card = None;
        if version >= 2 {
            let c = s.expect("pcic")?;
            let mut d = c.decoder(Pd6710::STATE_VERSION)?;
            pcic.load_state(&mut d)?;
            d.finish()?;
            let mut next = s.next_chunk()?;
            if let Some(c) = next.as_ref().filter(|c| c.name == "cf") {
                let mut d = c.decoder(CfCard::STATE_VERSION)?;
                let mut cf = CfCard::load_state(&mut d)?;
                d.finish()?;
                next = s.next_chunk()?;
                while let Some(c) = next.as_ref().filter(|c| c.name == "cf:blk") {
                    let mut d = c.decoder(1)?;
                    let i = d.u32()?;
                    let data = d.bytes()?;
                    cf.load_disk_block(i, data).map_err(Error::Format)?;
                    d.finish()?;
                    next = s.next_chunk()?;
                }
                card = Some(cf);
            }
            if let Some(c) = next {
                return format_err(format!("unexpected chunk {}", c.name));
            }
            if pcic.inserted() != card.is_some() {
                return format_err("pcic: socket state does not match the card");
            }
        } else {
            s.expect_end()?;
            let gpio = &mut b.stubs[StubId::Gpio as usize];
            if gpio.read(eint::EINTPEND, 4) != 0 {
                gpio.write(eint::EINTPEND, 4, 0);
            }
        }
        b.pcic = pcic;
        b.card = card;
        b.restore_eint();

        b.steps = steps;
        b.tick_acc = tick_acc;
        b.pending = 0;
        b.in_run = false;
        b.update_deadline();
        self.entry_pa = entry_pa;
        self.poll = None;
        self.sys.jit.flush(&mut self.sys.code);
        self.sys.code.reset();
        Ok(s.header.image_id.clone())
    }
}
