//! JIT とインタプリタの差分テスト（段階5。設計案 §8）。
//!
//! 生成コードの実行には wasm のホスト（JS の WebAssembly）が要るので、ネイティブや
//! wasm32-wasip1 の cargo test では回せない。web クレートがこの関数を export し、
//! Node で走らせる（rust/web/tests/jit-diff.mjs。tools/check.sh から呼ぶ）。
//!
//! 同じ初期状態の 2 台のマシン（片方だけ JIT を閾値 1・1 ブロックずつで有効にする）で、
//! ランダムな命令列を実行して、CPU 状態・停止の種類・RAM・ソフト TLB・MMU の
//! フォルト記録・UART を突き合わせる。命令列は JIT の対象の Op を中心に、対象外の
//! 命令・分岐・コードページへの書き込み・MMIO・MMU 有効（権限・FCSE・ユーザー
//! モード）を混ぜる。

use super::JitHost;
use crate::arm::{FLAG_C, FLAG_N, FLAG_V, FLAG_Z, MODE_SVC, MODE_USR, StopError};
use crate::mmu::Mmu;
use crate::smdk2410::Machine;

/// 試験に使う物理アドレス（MMU 有効時も同じ仮想アドレスで見える）。
const CODE: u32 = 0x3001_0000;
const CODE_PAGES: u32 = 4;
const DATA: u32 = 0x3010_0000;
const DATA_LEN: u32 = 0x1_0000;
/// 一次変換テーブル（16KB）
const TTB: u32 = 0x3020_0000;
/// 例外ハンドラ（VA 0 の section の行き先）
const HANDLER: u32 = 0x3030_0000;
/// 比べる RAM の範囲（上の全部を含む）
const CMP_LO: u32 = 0x3000_0000;
const CMP_HI: u32 = 0x3040_0000;
const UART1: u32 = 0x5000_4000;
/// FCSE の窓: MMU 有効時、VA 0x00100000〜（< 32MB なので MVA = PID | VA）を DATA に
/// 割り当てる。MMU 無効時は同じ VA がオープンバス（RAM でない）になる。
const LOW: u32 = 0x0010_0000;

/// 結果の要約。
#[derive(Clone, Debug, Default)]
pub struct Report {
    pub cases: u32,
    /// JIT 側で生成コードが実行した命令数・全命令数
    pub jit_executed: u64,
    pub total: u64,
    pub blocks: u64,
    pub side_exits: u64,
}

/// seed から cases 件の差分テストを行う。host は JIT のホストを作る。
/// 食い違ったら最初の食い違いの説明を Err で返す。
pub fn run(
    seed: u64,
    cases: u32,
    steps: u64,
    host: &mut dyn FnMut() -> Box<dyn JitHost>,
) -> Result<Report, String> {
    let mut a = Machine::new();
    let mut b = Machine::new();
    a.set_jit(Some(host()), 1, 1);
    let mut rep = Report::default();
    let mut rng = Rng(seed);
    for case in 0..cases {
        let setup = Setup::random(&mut rng);
        setup.apply(&mut a);
        setup.apply(&mut b);
        let ra = a.run_until(steps);
        let rb = b.run_until(steps);
        compare(&mut a, &mut b, ra, rb)
            .map_err(|e| format!("seed {seed} case {case} ({}): {e}", setup.describe()))?;
        rep.cases += 1;
        rep.total += a.steps();
    }
    let st = a.jit().stats();
    rep.jit_executed = st.executed;
    rep.blocks = st.blocks;
    rep.side_exits = st.side_exits;
    if let Some(e) = a.jit().error() {
        return Err(format!("jit disabled: {e}"));
    }
    // 対象の Op が全部コンパイルされたこと。
    let missing: Vec<String> = (0..=255u8)
        .filter_map(|o| {
            let op = super::codegen::op_by_number(o)?;
            (super::codegen::supported(op) && st.ops.get(o as usize).copied().unwrap_or(0) == 0)
                .then(|| format!("{op:?}"))
        })
        .collect();
    if !missing.is_empty() {
        return Err(format!("ops never compiled: {}", missing.join(" ")));
    }
    Ok(rep)
}

fn compare(
    a: &mut Machine,
    b: &mut Machine,
    ra: Result<(), StopError>,
    rb: Result<(), StopError>,
) -> Result<(), String> {
    if ra != rb {
        return Err(format!("stop: jit {ra:?} / interp {rb:?}"));
    }
    if a.cpu_dump() != b.cpu_dump() {
        return Err(format!(
            "cpu: jit {:08X?} / interp {:08X?}",
            a.cpu.arch_regs(),
            b.cpu.arch_regs()
        ));
    }
    let (ma, mb) = (&a.sys.mmu, &b.sys.mmu);
    if (ma.fsr, ma.far) != (mb.fsr, mb.far) {
        return Err(format!(
            "fault regs: jit {:X}/{:X} / interp {:X}/{:X}",
            ma.fsr, ma.far, mb.fsr, mb.far
        ));
    }
    for (i, (ea, eb)) in ma.tlb.iter().zip(mb.tlb.iter()).enumerate() {
        if (ea.tag, ea.pa, ea.perm) != (eb.tag, eb.pa, eb.perm) {
            return Err(format!("tlb[{i}]: jit {ea:?} / interp {eb:?}"));
        }
    }
    let ram = |m: &Machine| {
        let (r, off) = m.sys.bus.ram(CMP_LO).expect("ram");
        r[off as usize..(off + (CMP_HI - CMP_LO)) as usize].to_vec()
    };
    let (xa, xb) = (ram(a), ram(b));
    if xa != xb {
        let i = xa.iter().zip(&xb).position(|(p, q)| p != q).unwrap_or(0);
        return Err(format!(
            "ram at {:08X}: jit {:02X} / interp {:02X}",
            CMP_LO + i as u32,
            xa[i],
            xb[i]
        ));
    }
    if a.take_uart1() != b.take_uart1() {
        return Err("uart1 output".into());
    }
    Ok(())
}

/// 1 件の初期状態。
struct Setup {
    code: Vec<u32>,
    data: Vec<u8>,
    regs: [u32; 15],
    cpsr: u32,
    mmu: Option<MmuSetup>,
}

struct MmuSetup {
    ctrl: u32,
    dacr: u32,
    pid: u32,
    /// DATA の section の AP
    data_ap: u32,
}

impl Setup {
    fn random(rng: &mut Rng) -> Setup {
        let n = (CODE_PAGES * 1024) as usize;
        let code = (0..n).map(|_| gen_word(rng)).collect();
        let data = (0..DATA_LEN).map(|_| rng.u32() as u8).collect();
        let mut regs = [0u32; 15];
        for (i, r) in regs.iter_mut().enumerate() {
            *r = match i {
                0 | 1 => DATA + rng.below(DATA_LEN),
                4 => {
                    if rng.below(2) == 0 {
                        LOW + rng.below(DATA_LEN)
                    } else {
                        DATA + rng.below(DATA_LEN)
                    }
                }
                2 => CODE + rng.below(CODE_PAGES * 4096),
                3 => {
                    if rng.below(3) == 0 {
                        UART1
                    } else {
                        DATA + rng.below(DATA_LEN)
                    }
                }
                5 => {
                    // BX・MOV pc の行き先（まれに Thumb）
                    let t = CODE + 4 * rng.below(CODE_PAGES * 1024);
                    t | (rng.below(10) == 0) as u32
                }
                _ => rng.u32(),
            };
        }
        let flags = rng.u32() & (FLAG_N | FLAG_Z | FLAG_C | FLAG_V);
        let mmu = (rng.below(2) == 0).then(|| MmuSetup {
            // M に S・R をランダムに（AP=0 の意味が変わる）
            ctrl: 1 | (rng.below(4) << 8),
            dacr: if rng.below(4) == 0 { 3 } else { 1 },
            pid: [0, 0x0200_0000, 0x7E00_0000][rng.below(3) as usize],
            data_ap: rng.below(4),
        });
        let user = mmu.is_some() && rng.below(3) == 0;
        let cpsr = flags | if user { MODE_USR } else { MODE_SVC };
        Setup {
            code,
            data,
            regs,
            cpsr,
            mmu,
        }
    }

    fn describe(&self) -> String {
        match &self.mmu {
            None => format!("mmu off, cpsr {:08X}", self.cpsr),
            Some(m) => format!(
                "mmu ctrl {:X} dacr {:X} pid {:X} data ap {}, cpsr {:08X}",
                m.ctrl, m.dacr, m.pid, m.data_ap, self.cpsr
            ),
        }
    }

    fn apply(&self, m: &mut Machine) {
        let w32 = |m: &mut Machine, pa: u32, v: u32| {
            let (r, off) = m.sys.bus.ram_mut(pa).expect("ram");
            r[off as usize..off as usize + 4].copy_from_slice(&v.to_le_bytes());
        };
        {
            let (r, off) = m.sys.bus.ram_mut(CMP_LO).expect("ram");
            r[off as usize..(off + (CMP_HI - CMP_LO)) as usize].fill(0);
        }
        for (i, w) in self.code.iter().enumerate() {
            w32(m, CODE + 4 * i as u32, *w);
        }
        {
            let (r, off) = m.sys.bus.ram_mut(DATA).expect("ram");
            r[off as usize..(off + DATA_LEN) as usize].copy_from_slice(&self.data);
        }
        // 例外ハンドラ: どの例外も SUBS pc, lr, #4 で戻る（データアボートは
        // 次の次の命令へ、未定義・SWI は 1 つ先の命令の次へ。どちらでも両者で同じ）。
        for v in 0..8 {
            w32(m, HANDLER + 4 * v, 0xE25EF004);
        }
        m.sys.mmu = Mmu::new();
        m.take_uart1();
        m.entry_pa = CODE;
        m.reset();
        if let Some(mm) = &self.mmu {
            // section（1MB）の記述子: PA | AP << 10 | domain 0 | 0b10
            let sect = |pa: u32, ap: u32| pa & 0xFFF0_0000 | ap << 10 | 2;
            for mb in 0x300..0x380u32 {
                let ap = if mb == DATA >> 20 { mm.data_ap } else { 3 };
                w32(m, TTB + 4 * mb, sect(mb << 20, ap));
            }
            w32(m, TTB + 4 * (UART1 >> 20), sect(UART1, 3));
            // VA 0（ベクタ）と FCSE で移る先（MVA = PID | VA）
            w32(m, TTB, sect(HANDLER, 3));
            w32(m, TTB + 4 * (mm.pid >> 20), sect(HANDLER, 3));
            w32(m, TTB + 4 * ((mm.pid | LOW) >> 20), sect(DATA, mm.data_ap));
            let mmu = &mut m.sys.mmu;
            mmu.cp15_write(0, 2, 0, 0, TTB);
            mmu.cp15_write(0, 3, 0, 0, mm.dacr);
            mmu.cp15_write(0, 13, 0, 0, mm.pid);
            mmu.cp15_write(0, 1, 0, 0, mm.ctrl);
        }
        for (i, r) in self.regs.iter().enumerate() {
            m.cpu.set_reg(i, *r);
        }
        m.cpu.set_cpsr(self.cpsr, &mut m.sys);
    }
}

/// 命令語 1 個（JIT の対象を中心に、分岐・対象外を混ぜる）。
fn gen_word(rng: &mut Rng) -> u32 {
    let cond = if rng.below(5) == 0 {
        rng.below(15)
    } else {
        0xE
    } << 28;
    // 書き込み先は r6〜r12（ベースの r0〜r5 を壊しにくくする）。
    let dst = |rng: &mut Rng| 6 + rng.below(7);
    let src = |rng: &mut Rng| rng.below(15);
    match rng.below(20) {
        // データ処理（即値）
        0..=3 => {
            let op = rng.below(16);
            let s = if (8..=11).contains(&op) {
                1
            } else {
                rng.below(2)
            };
            cond | 1 << 25
                | op << 21
                | s << 20
                | src(rng) << 16
                | dst(rng) << 12
                | if rng.below(2) == 0 {
                    rng.below(256) // 回転なし（C を変えない形）
                } else {
                    rng.below(4096)
                }
        }
        // データ処理（レジスタ・シフトなし／即値シフト）
        4..=6 => {
            let op = rng.below(16);
            let s = if (8..=11).contains(&op) {
                1
            } else {
                rng.below(2)
            };
            let shift = if rng.below(2) == 0 {
                0
            } else {
                rng.below(32) << 7 | rng.below(4) << 5
            };
            cond | op << 21 | s << 20 | src(rng) << 16 | dst(rng) << 12 | shift | src(rng)
        }
        // ベースのポインタを小さく進める（ADD/SUB r0〜r4, r0〜r4, #小）
        7 => {
            let r = rng.below(5);
            let op = if rng.below(2) == 0 { 4 } else { 2 };
            cond | 1 << 25 | op << 21 | r << 16 | r << 12 | rng.below(64)
        }
        // LDR/STR/LDRB/STRB 即値（ベースは r0〜r4、まれに PC）
        8..=12 => {
            let load = rng.below(2);
            let byte = rng.below(2);
            let base = if rng.below(10) == 0 { 15 } else { rng.below(5) };
            let rt = if load == 1 { dst(rng) } else { src(rng) };
            let up = rng.below(2);
            let off = if rng.below(4) == 0 {
                rng.below(4096)
            } else {
                rng.below(64)
            };
            cond | 0x0500_0000 | up << 23 | byte << 22 | load << 20 | base << 16 | rt << 12 | off
        }
        // B/BL（コード領域の中に収まりやすい小さな差分）
        13..=14 => {
            let l = rng.below(4) == 0;
            let off = (rng.below(64) as i32 - 40) as u32 & 0x00FF_FFFF;
            cond | 0x0A00_0000 | (l as u32) << 24 | off
        }
        // BX r5 / MOV pc, r5
        15 => {
            if rng.below(2) == 0 {
                cond | 0x012F_FF15
            } else {
                cond | 0x01A0_F005
            }
        }
        // 対象外: レジスタ指定シフト・乗算・LDM/STM（r0/r1 のデータ領域）・MRS・
        // ライトバックつき LDR/STR・ハーフワード
        16 => {
            let op = rng.below(16);
            let s = ((8..=11).contains(&op) || rng.below(2) == 0) as u32;
            cond | 0x10
                | op << 21
                | s << 20
                | src(rng) << 16
                | dst(rng) << 12
                | src(rng) << 8
                | rng.below(4) << 5
                | src(rng)
        }
        17 => cond | 0x0000_0090 | dst(rng) << 16 | src(rng) << 8 | src(rng),
        18 => {
            let base = rng.below(2);
            cond | 0x0800_0000 | rng.below(2) << 20 | base << 16 | (rng.u32() & 0x1FC0)
        }
        _ => match rng.below(3) {
            0 => cond | 0x010F_0000 | dst(rng) << 12,
            1 => cond | 0x0490_0004 | rng.below(5) << 16 | dst(rng) << 12,
            _ => cond | 0x01D0_00B2 | rng.below(5) << 16 | dst(rng) << 12,
        },
    }
}

/// 決定論的な乱数（SplitMix64）。
struct Rng(u64);

impl Rng {
    fn u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn u32(&mut self) -> u32 {
        (self.u64() >> 32) as u32
    }
    fn below(&mut self, n: u32) -> u32 {
        ((self.u32() as u64 * n as u64) >> 32) as u32
    }
}
