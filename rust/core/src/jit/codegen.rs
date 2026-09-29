//! ブロックの切り出しと、IR → wasm 関数の生成（段階5。設計は docs/stage5-design.md）。
//!
//! 生成関数の約束（`(ctx) -> 結果`）:
//!   - 入口で「残りの命令数 ≥ ブロック長」を確かめ、足りなければ何もせず
//!     `SIDE | 0` を返す。
//!   - 命令は IR（ir.rs）の意味をそのまま写す。1 IR 命令 = 1 ARM 命令。条件不成立の
//!     命令も 1 命令と数える（インタプリタと同じ）。
//!   - ロード・ストアはソフト TLB のヒット（権限あり・RAM・ストアなら wram あり）の
//!     ときだけ行う。それ以外はその命令の手前で戻る（サイド出口: 書き換えた
//!     レジスタと CPSR を書き戻し、regs[15] にその命令の PC を入れて `SIDE | k` を返す。
//!     k は実行を終えた命令数）。インタプリタがその命令から続けるので、MMIO・TLB の
//!     埋め方・フォルト・コードページへの書き込みの検出はインタプリタの経路で起きる。
//!     TLB は読むだけで変えない。
//!   - 最後まで実行したら regs[15] に次の PC を入れて k（= ブロック長）を返す。
//!
//! PC はブロックに入ったときの regs[15] を基準にした差分で扱う（同じ物理ページが
//! 別の仮想アドレスに見えていても、同じ生成コードが正しく動くように）。
//!
//! 生成コードは割り込み線・期限を変える命令（MMIO・CPSR のモード・MCR）を含まない
//! ので、ブロックに入る前に Rust が確かめた条件（割り込みなし・上限の残り）は
//! ブロックの間ずっと成り立つ。BX で Thumb に切り替わることはある（呼び出し側が
//! 戻った後に確かめる）。

use super::JitCtx;
use super::wasm::{Func, Local};
use crate::arm::{FLAG_T, Instr, Op};
use crate::mmu::{NO_RAM, TLB_SIZE, TLB_VALID, TlbEntry};

/// ブロックの最大の長さ（命令数）。生成関数を小さく保つため。
pub(crate) const MAX_BLOCK: usize = 64;

/// 戻り値のサイド出口の印（下位 16 ビットが実行した命令数）。
pub(crate) const SIDE: u32 = 1 << 16;

/// 5-1 で JIT する Op（特化済みの単純なもの）。
pub(crate) fn supported(op: Op) -> bool {
    use Op::*;
    matches!(
        op,
        MovImm
            | AddImm
            | SubImm
            | AndImm
            | OrrImm
            | EorImm
            | BicImm
            | CmpImm
            | CmnImm
            | SubsImm
            | AddsImm
            | TstImm
            | TstImmC
            | TeqImm
            | TeqImmC
            | AndsImm
            | AndsImmC
            | OrrsImm
            | OrrsImmC
            | MovsImm
            | MovsImmC
            | MovReg
            | AddReg
            | SubReg
            | AndReg
            | OrrReg
            | EorReg
            | BicReg
            | CmpReg
            | SubsReg
            | AddsReg
            | MovsReg
            | TstReg
            | MovShift
            | MvnShift
            | AddShift
            | SubShift
            | RsbShift
            | AndShift
            | OrrShift
            | EorShift
            | BicShift
            | MovPcReg
            | BxReg
            | LdrImm
            | LdrbImm
            | StrImm
            | StrbImm
            | B
            | Bl
    )
}

/// PC を書く（ブロックを終える）Op。
pub(crate) fn terminates(op: Op) -> bool {
    matches!(op, Op::B | Op::Bl | Op::BxReg | Op::MovPcReg)
}

/// ページ内の命令 idx（0〜1023）から始まるブロックを切り出す。instr_at は
/// ページ内の命令番号の命令を返す。対象外の命令・PC を書く命令（含める）・
/// ページ末尾・MAX_BLOCK で終わる。先頭が対象外なら空。
pub(crate) fn form_block(idx: u32, mut instr_at: impl FnMut(u32) -> Instr) -> Vec<Instr> {
    let mut v = vec![];
    let mut i = idx;
    while i < 1024 && v.len() < MAX_BLOCK {
        let ins = instr_at(i);
        if !supported(ins.op) {
            break;
        }
        v.push(ins);
        if terminates(ins.op) {
            break;
        }
        i += 1;
    }
    v
}

/// 特化した Op の一覧（Op の番号の順。試験で全 Op を試したかを見るのに使う）。
const SPECIAL_OPS: &[Op] = &[
    Op::MovImm,
    Op::AddImm,
    Op::SubImm,
    Op::AndImm,
    Op::OrrImm,
    Op::EorImm,
    Op::BicImm,
    Op::CmpImm,
    Op::CmnImm,
    Op::SubsImm,
    Op::AddsImm,
    Op::TstImm,
    Op::TstImmC,
    Op::TeqImm,
    Op::TeqImmC,
    Op::AndsImm,
    Op::AndsImmC,
    Op::OrrsImm,
    Op::OrrsImmC,
    Op::MovsImm,
    Op::MovsImmC,
    Op::MovReg,
    Op::AddReg,
    Op::SubReg,
    Op::AndReg,
    Op::OrrReg,
    Op::EorReg,
    Op::BicReg,
    Op::CmpReg,
    Op::SubsReg,
    Op::AddsReg,
    Op::MovsReg,
    Op::TstReg,
    Op::MovShift,
    Op::MvnShift,
    Op::AddShift,
    Op::SubShift,
    Op::RsbShift,
    Op::AndShift,
    Op::OrrShift,
    Op::EorShift,
    Op::BicShift,
    Op::MovPcReg,
    Op::BxReg,
    Op::LdrhImm,
    Op::LdrsbImm,
    Op::LdrshImm,
    Op::StrhImm,
    Op::LdrPre,
    Op::LdrPost,
    Op::LdrbPre,
    Op::LdrbPost,
    Op::StrPre,
    Op::StrPost,
    Op::StrbPre,
    Op::StrbPost,
    Op::LdrImm,
    Op::LdrbImm,
    Op::StrImm,
    Op::StrbImm,
    Op::B,
    Op::Bl,
    Op::BSpin,
];

/// Op の番号から特化した Op を引く（汎用の Op は JIT の対象外なので None）。
pub(crate) fn op_by_number(o: u8) -> Option<Op> {
    SPECIAL_OPS.iter().copied().find(|op| *op as u8 == o)
}

// ---- ローカル変数の割り当て ----
const CTX: Local = 0;
const REGS: Local = 1;
const TLB: Local = 2;
const ARENA: Local = 3;
const PID: Local = 4;
const PERM_R: Local = 5;
const PERM_W: Local = 6;
const CPSR_A: Local = 7;
const CPSR: Local = 8;
const NPC: Local = 9;
const PC0: Local = 10;
const T0: Local = 11;
const T1: Local = 12;
const T2: Local = 13;
const T3: Local = 14;
/// r0〜r14（r15 はローカルに持たない。読みは PC0 からの差分）
const R0: Local = 15;
const NUM_LOCALS: u32 = R0 + 15;

const fn r(n: u8) -> Local {
    R0 + n as Local
}

// ---- 構造体のオフセット（生成時に埋め込む。配置は Rust に任せる）----
const CTX_REGS: u32 = std::mem::offset_of!(JitCtx, regs) as u32;
const CTX_CPSR: u32 = std::mem::offset_of!(JitCtx, cpsr) as u32;
const CTX_TLB: u32 = std::mem::offset_of!(JitCtx, tlb) as u32;
const CTX_ARENA: u32 = std::mem::offset_of!(JitCtx, arena) as u32;
const CTX_PID: u32 = std::mem::offset_of!(JitCtx, pid) as u32;
const CTX_PERM_R: u32 = std::mem::offset_of!(JitCtx, perm_r) as u32;
const CTX_PERM_W: u32 = std::mem::offset_of!(JitCtx, perm_w) as u32;
const CTX_REMAINING: u32 = std::mem::offset_of!(JitCtx, remaining) as u32;
const TLB_ESZ: u32 = std::mem::size_of::<TlbEntry>() as u32;
const TLB_TAG: u32 = std::mem::offset_of!(TlbEntry, tag) as u32;
const TLB_PERM: u32 = std::mem::offset_of!(TlbEntry, perm) as u32;
const TLB_RAM: u32 = std::mem::offset_of!(TlbEntry, ram) as u32;
const TLB_WRAM: u32 = std::mem::offset_of!(TlbEntry, wram) as u32;

// 生成コードは TlbEntry の各フィールドを i32/u8 として読む。
const _: () = assert!(std::mem::size_of::<crate::bus::RamOff>() == 4);

/// 命令が読むレジスタ（r0〜r14 のビット集合。r15 は含めない）と書くレジスタ。
fn reads_writes(i: &Instr) -> (u16, u16) {
    use Op::*;
    let bit = |n: u32| if n < 15 { 1u16 << n } else { 0 };
    let (rd, rn) = (bit(i.rd as u32), bit(i.rn as u32));
    let rm = bit(i.imm & 0xF);
    match i.op {
        MovImm | MovsImm | MovsImmC => (0, rd),
        AddImm | SubImm | AndImm | OrrImm | EorImm | BicImm | SubsImm | AddsImm | AndsImm
        | AndsImmC | OrrsImm | OrrsImmC => (rn, rd),
        CmpImm | CmnImm | TstImm | TstImmC | TeqImm | TeqImmC => (rn, 0),
        MovReg | MovsReg | MovShift | MvnShift => (rm, rd),
        AddReg | SubReg | AndReg | OrrReg | EorReg | BicReg | SubsReg | AddsReg | AddShift
        | SubShift | RsbShift | AndShift | OrrShift | EorShift | BicShift => (rn | rm, rd),
        CmpReg | TstReg => (rn | rm, 0),
        MovPcReg => (rm, 0),
        BxReg => (rn, 0),
        LdrImm | LdrbImm => (rn, rd),
        StrImm | StrbImm => (rn | rd, 0),
        Bl => (0, bit(14)),
        _ => (0, 0),
    }
}

/// CPSR を読み書きするか（条件・フラグ・BX の T）。
fn uses_cpsr(i: &Instr) -> bool {
    i.cond != 0xE || sets_cpsr(i)
}

fn sets_cpsr(i: &Instr) -> bool {
    use Op::*;
    matches!(
        i.op,
        CmpImm
            | CmnImm
            | SubsImm
            | AddsImm
            | TstImm
            | TstImmC
            | TeqImm
            | TeqImmC
            | AndsImm
            | AndsImmC
            | OrrsImm
            | OrrsImmC
            | MovsImm
            | MovsImmC
            | CmpReg
            | SubsReg
            | AddsReg
            | MovsReg
            | TstReg
            | BxReg
    )
}

fn is_mem(op: Op) -> bool {
    matches!(op, Op::LdrImm | Op::LdrbImm | Op::StrImm | Op::StrbImm)
}

struct Gen {
    f: Func,
    /// 書き換えたレジスタ（出口で書き戻す）
    dirty: u16,
    /// CPSR を書き換えた
    cpsr_dirty: bool,
}

/// ブロック（form_block の結果、空でない）の wasm 関数を作る。
pub(crate) fn gen_block(block: &[Instr]) -> Func {
    let len = block.len() as u32;
    let mut used = 0u16;
    let (mut cpsr, mut mem) = (false, false);
    for i in block {
        let (rd, wr) = reads_writes(i);
        used |= rd | wr;
        cpsr |= uses_cpsr(i);
        mem |= is_mem(i.op);
    }
    let mut g = Gen {
        f: Func::new(NUM_LOCALS),
        dirty: 0,
        cpsr_dirty: false,
    };
    let f = &mut g.f;
    // 上限の確認（ブロックの途中では数えない）。
    f.get(CTX).load(CTX_REMAINING).i32(len).lt_u();
    f.if_().i32(SIDE).ret().end();
    f.get(CTX).load(CTX_REGS).tee(REGS).load(60).set(PC0);
    if cpsr {
        f.get(CTX).load(CTX_CPSR).tee(CPSR_A).load(0).set(CPSR);
    }
    if mem {
        f.get(CTX).load(CTX_TLB).set(TLB);
        f.get(CTX).load(CTX_ARENA).set(ARENA);
        f.get(CTX).load(CTX_PID).set(PID);
        f.get(CTX).load(CTX_PERM_R).set(PERM_R);
        f.get(CTX).load(CTX_PERM_W).set(PERM_W);
    }
    for n in 0..15u8 {
        if used & (1 << n) != 0 {
            f.get(REGS).load(4 * n as u32).set(r(n));
        }
    }
    let mut npc = false;
    for (k, i) in block.iter().enumerate() {
        let k = k as u32;
        if terminates(i.op) {
            // 条件不成立なら次の命令へ。
            g.pc(k + 1);
            g.f.set(NPC);
            npc = true;
        }
        let cond = i.cond != 0xE;
        if cond {
            g.cond(i.cond);
            g.f.if_();
        }
        g.instr(k, i);
        if cond {
            g.f.end();
        }
        g.dirty |= reads_writes(i).1;
        g.cpsr_dirty |= sets_cpsr(i);
    }
    g.exit(len, false, npc);
    g.f
}

impl Gen {
    /// ブロック先頭から k 命令目の PC を積む。
    fn pc(&mut self, k: u32) {
        self.f.get(PC0);
        if k != 0 {
            self.f.i32(4 * k).add();
        }
    }

    /// レジスタ n の値を積む（r15 は実行中の regs[15] = PC+4。IR の実行と同じ）。
    fn reg(&mut self, k: u32, n: u8) {
        if n == 15 {
            self.pc(k + 1);
        } else {
            self.f.get(r(n));
        }
    }

    /// 出口: 書き換えたものを書き戻して戻る。k は実行を終えた命令数。npc なら
    /// regs[15] に NPC を、でなければ k 命令目の PC を入れる。
    fn exit(&mut self, k: u32, side: bool, npc: bool) {
        for n in 0..15u8 {
            if self.dirty & (1 << n) != 0 {
                self.f.get(REGS).get(r(n)).store(4 * n as u32);
            }
        }
        if self.cpsr_dirty {
            self.f.get(CPSR_A).get(CPSR).store(0);
        }
        self.f.get(REGS);
        if npc {
            self.f.get(NPC);
        } else {
            self.pc(k);
        }
        self.f.store(60);
        self.f.i32(k | if side { SIDE } else { 0 }).ret();
    }

    /// 条件（cond_passed）を 0/1 で積む。
    fn cond(&mut self, c: u8) {
        // N=31 Z=30 C=29 V=28
        let f = &mut self.f;
        let bit = |f: &mut Func, b: u32| {
            f.get(CPSR).i32(b).shr_u().i32(1).and();
        };
        // N^V を bit0 に
        let nxv = |f: &mut Func| {
            f.get(CPSR).i32(31).shr_u().get(CPSR).i32(28).shr_u().xor();
        };
        match c {
            0x0 => bit(f, 30),
            0x1 => {
                bit(f, 30);
                f.eqz();
            }
            0x2 => bit(f, 29),
            0x3 => {
                bit(f, 29);
                f.eqz();
            }
            0x4 => {
                f.get(CPSR).i32(31).shr_u();
            }
            0x5 => {
                f.get(CPSR).i32(31).shr_u().eqz();
            }
            0x6 => bit(f, 28),
            0x7 => {
                bit(f, 28);
                f.eqz();
            }
            0x8 | 0x9 => {
                // HI: C && !Z
                f.get(CPSR).i32(29).shr_u();
                f.get(CPSR).i32(30).shr_u().i32(u32::MAX).xor();
                f.and().i32(1).and();
                if c == 0x9 {
                    f.eqz();
                }
            }
            0xA | 0xB => {
                nxv(f);
                f.i32(1).and();
                if c == 0xA {
                    f.eqz();
                }
            }
            _ => {
                // GT: !Z && N==V。LE はその否定。
                nxv(f);
                f.get(CPSR).i32(30).shr_u().or().i32(1).and();
                if c == 0xC {
                    f.eqz();
                }
            }
        }
    }

    /// N・Z を結果 res（ローカル）から入れる。mask は残す CPSR のビット、extra は
    /// 足すビット（C の定数など）。
    fn set_nz(&mut self, res: Local, mask: u32, extra: u32) {
        let f = &mut self.f;
        f.get(CPSR).i32(mask).and();
        f.get(res).i32(0x8000_0000).and().or();
        f.get(res).eqz().i32(30).shl().or();
        if extra != 0 {
            f.i32(extra).or();
        }
        f.set(CPSR);
    }

    /// 加減算のフラグ 4 つ（n=T0・m=T2・結果=T1）。
    fn set_nzcv(&mut self, add: bool) {
        let f = &mut self.f;
        f.get(CPSR).i32(0x0FFF_FFFF).and();
        f.get(T1).i32(0x8000_0000).and().or();
        f.get(T1).eqz().i32(30).shl().or();
        if add {
            f.get(T1).get(T0).lt_u(); // 桁上がり
        } else {
            f.get(T0).get(T2).ge_u(); // 借りなし
        }
        f.i32(29).shl().or();
        if add {
            f.get(T0).get(T1).xor().get(T2).get(T1).xor().and();
        } else {
            f.get(T0).get(T2).xor().get(T0).get(T1).xor().and();
        }
        f.i32(31).shr_u().i32(28).shl().or();
        f.set(CPSR);
    }

    /// 即値シフトつきのレジスタオペランド（imm = Rm | 種類 << 4 | 量 << 8、量は 1〜31）を積む。
    fn shifted(&mut self, k: u32, imm: u32) {
        self.reg(k, (imm & 0xF) as u8);
        self.f.i32(imm >> 8);
        match (imm >> 4) & 3 {
            0 => self.f.shl(),
            1 => self.f.shr_u(),
            2 => self.f.shr_s(),
            _ => self.f.rotr(),
        };
    }

    /// ソフト TLB を引き、ヒット（権限あり・RAM）ならアリーナ上のアドレスを積む。
    /// 仮想アドレスは T1（ワードなら 4 の倍数に揃えたもの）。外れならサイド出口。
    fn mem_addr(&mut self, k: u32, write: bool) {
        let f = &mut self.f;
        // MVA（FCSE: VA < 32MB は PID を OR する）
        f.get(T1)
            .get(PID)
            .or()
            .get(T1)
            .get(T1)
            .i32(0x0200_0000)
            .lt_u()
            .select();
        f.set(T2);
        // エントリ（直接マップ、添字は MVA[21:12]）
        f.get(T2)
            .i32(12)
            .shr_u()
            .i32(TLB_SIZE as u32 - 1)
            .and()
            .i32(TLB_ESZ)
            .mul();
        f.get(TLB).add().tee(T3);
        f.load(TLB_TAG)
            .get(T2)
            .i32(12)
            .shr_u()
            .i32(TLB_VALID)
            .or()
            .ne();
        f.if_();
        self.exit(k, true, false);
        self.f.end();
        let f = &mut self.f;
        f.get(T3)
            .load8_u(TLB_PERM)
            .get(if write { PERM_W } else { PERM_R })
            .and()
            .eqz();
        f.if_();
        self.exit(k, true, false);
        self.f.end();
        let f = &mut self.f;
        f.get(T3)
            .load(if write { TLB_WRAM } else { TLB_RAM })
            .tee(T3)
            .i32(NO_RAM)
            .eq();
        f.if_();
        self.exit(k, true, false);
        self.f.end();
        let f = &mut self.f;
        f.get(ARENA).get(T3).add().get(T1).i32(0xFFF).and().add();
    }

    /// 1 命令（条件は呼び出し側）。
    fn instr(&mut self, k: u32, i: &Instr) {
        use Op::*;
        let (rd, rn, imm) = (i.rd, i.rn, i.imm);
        let rm = (imm & 0xF) as u8;
        match i.op {
            MovImm => {
                self.f.i32(imm).set(r(rd));
            }
            AddImm | SubImm | AndImm | OrrImm | EorImm | BicImm => {
                self.reg(k, rn);
                match i.op {
                    AddImm => self.f.i32(imm).add(),
                    SubImm => self.f.i32(imm).sub(),
                    AndImm => self.f.i32(imm).and(),
                    OrrImm => self.f.i32(imm).or(),
                    EorImm => self.f.i32(imm).xor(),
                    _ => self.f.i32(!imm).and(),
                };
                self.f.set(r(rd));
            }
            CmpImm | CmnImm | SubsImm | AddsImm => {
                let add = matches!(i.op, CmnImm | AddsImm);
                self.reg(k, rn);
                self.f.set(T0);
                self.f.i32(imm).set(T2);
                self.f.get(T0).get(T2);
                if add {
                    self.f.add()
                } else {
                    self.f.sub()
                };
                self.f.set(T1);
                self.set_nzcv(add);
                if matches!(i.op, SubsImm | AddsImm) {
                    self.f.get(T1).set(r(rd));
                }
            }
            TstImm | TstImmC | TeqImm | TeqImmC | AndsImm | AndsImmC | OrrsImm | OrrsImmC
            | MovsImm | MovsImmC => {
                match i.op {
                    TstImm | TstImmC | AndsImm | AndsImmC => {
                        self.reg(k, rn);
                        self.f.i32(imm).and();
                    }
                    TeqImm | TeqImmC => {
                        self.reg(k, rn);
                        self.f.i32(imm).xor();
                    }
                    OrrsImm | OrrsImmC => {
                        self.reg(k, rn);
                        self.f.i32(imm).or();
                    }
                    _ => {
                        self.f.i32(imm);
                    }
                }
                self.f.set(T1);
                if matches!(i.op, TstImmC | TeqImmC | AndsImmC | OrrsImmC | MovsImmC) {
                    // 回転ありの即値: C = 即値の bit31
                    self.set_nz(T1, 0x1FFF_FFFF, if imm >> 31 != 0 { 1 << 29 } else { 0 });
                } else {
                    self.set_nz(T1, 0x3FFF_FFFF, 0);
                }
                if !matches!(i.op, TstImm | TstImmC | TeqImm | TeqImmC) {
                    self.f.get(T1).set(r(rd));
                }
            }
            MovReg => {
                self.reg(k, rm);
                self.f.set(r(rd));
            }
            AddReg | SubReg | AndReg | OrrReg | EorReg | BicReg => {
                self.reg(k, rn);
                self.reg(k, rm);
                match i.op {
                    AddReg => self.f.add(),
                    SubReg => self.f.sub(),
                    AndReg => self.f.and(),
                    OrrReg => self.f.or(),
                    EorReg => self.f.xor(),
                    _ => self.f.i32(u32::MAX).xor().and(),
                };
                self.f.set(r(rd));
            }
            CmpReg | SubsReg | AddsReg => {
                let add = i.op == AddsReg;
                self.reg(k, rn);
                self.f.set(T0);
                self.reg(k, rm);
                self.f.set(T2);
                self.f.get(T0).get(T2);
                if add {
                    self.f.add()
                } else {
                    self.f.sub()
                };
                self.f.set(T1);
                self.set_nzcv(add);
                if i.op != CmpReg {
                    self.f.get(T1).set(r(rd));
                }
            }
            MovsReg => {
                self.reg(k, rm);
                self.f.set(T1);
                self.set_nz(T1, 0x3FFF_FFFF, 0);
                self.f.get(T1).set(r(rd));
            }
            TstReg => {
                self.reg(k, rn);
                self.reg(k, rm);
                self.f.and().set(T1);
                self.set_nz(T1, 0x3FFF_FFFF, 0);
            }
            MovShift | MvnShift => {
                self.shifted(k, imm);
                if i.op == MvnShift {
                    self.f.i32(u32::MAX).xor();
                }
                self.f.set(r(rd));
            }
            AddShift | SubShift | RsbShift | AndShift | OrrShift | EorShift | BicShift => {
                if i.op == RsbShift {
                    self.shifted(k, imm);
                    self.reg(k, rn);
                    self.f.sub();
                } else {
                    self.reg(k, rn);
                    self.shifted(k, imm);
                    match i.op {
                        AddShift => self.f.add(),
                        SubShift => self.f.sub(),
                        AndShift => self.f.and(),
                        OrrShift => self.f.or(),
                        EorShift => self.f.xor(),
                        _ => self.f.i32(u32::MAX).xor().and(),
                    };
                }
                self.f.set(r(rd));
            }
            MovPcReg => {
                self.reg(k, rm);
                self.f.i32(!3).and().set(NPC);
            }
            BxReg => {
                self.reg(k, rn);
                let f = &mut self.f;
                f.tee(T0).i32(1).and();
                f.if_();
                f.get(CPSR).i32(FLAG_T).or().set(CPSR);
                f.get(T0).i32(!1).and().set(NPC);
                f.else_();
                f.get(CPSR).i32(!FLAG_T).and().set(CPSR);
                f.get(T0).i32(!3).and().set(NPC);
                f.end();
            }
            LdrImm | LdrbImm | StrImm | StrbImm => {
                let word = matches!(i.op, LdrImm | StrImm);
                let write = matches!(i.op, StrImm | StrbImm);
                self.reg(k, rn);
                self.f.i32(imm).add().tee(T0);
                if word {
                    self.f.i32(!3).and();
                }
                self.f.set(T1);
                self.mem_addr(k, write);
                match i.op {
                    LdrImm => {
                        // 非アラインは ARMv4 の回転（ワード境界から読んで 8×(addr&3) 回す）
                        self.f.load(0).get(T0).i32(3).and().i32(3).shl().rotr();
                        self.f.set(r(rd));
                    }
                    LdrbImm => {
                        self.f.load8_u(0).set(r(rd));
                    }
                    StrImm => {
                        self.reg(k, rd);
                        self.f.store(0);
                    }
                    _ => {
                        self.reg(k, rd);
                        self.f.store8(0);
                    }
                }
            }
            B | Bl => {
                if i.op == Bl {
                    self.pc(k + 1);
                    self.f.set(r(14));
                }
                // imm は regs[15]（= PC+4）に足す差分
                self.f
                    .get(PC0)
                    .i32((4 * k + 4).wrapping_add(imm))
                    .add()
                    .set(NPC);
            }
            _ => unreachable!("gen_block: unsupported op {:?}", i.op),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arm::decode_instr;

    #[test]
    fn block_stops_at_branch_and_unsupported() {
        // MOV r0,#1; ADD r1,r0,#2; B .; MOV r2,#3
        let words = [0xE3A00001, 0xE2801002, 0xEAFFFFFE, 0xE3A02003];
        let b = form_block(0, |i| decode_instr(words[i as usize]));
        assert_eq!(b.len(), 3);
        // SWI は対象外
        let words = [0xE3A00001, 0xEF000000];
        let b = form_block(0, |i| decode_instr(words[i as usize]));
        assert_eq!(b.len(), 1);
        let b = form_block(1, |i| decode_instr(words[i as usize]));
        assert!(b.is_empty());
    }

    #[test]
    fn block_stops_at_page_end() {
        let b = form_block(1020, |_| decode_instr(0xE3A00001));
        assert_eq!(b.len(), 4);
        let b = form_block(0, |_| decode_instr(0xE3A00001));
        assert_eq!(b.len(), MAX_BLOCK);
    }

    /// 対象の Op がすべて生成できる（生成で panic しない）。実行の確かめは Node の
    /// 差分テスト（jit::selftest）。
    #[test]
    fn all_supported_ops_generate() {
        let words = [
            0xE3A00001, 0x13A00001, 0xE2811002, 0xE2411002, 0xE2011002, 0xE3811002, 0xE2211002,
            0xE3C11002, 0xE3510002, 0xE3710002, 0xE2511002, 0xE2911002, 0xE3110002, 0xE3110102,
            0xE3310002, 0xE3310102, 0xE2111002, 0xE2111102, 0xE3911002, 0xE3911102, 0xE3B01002,
            0xE3B01102, 0xE1A01002, 0xE0811002, 0xE0411002, 0xE0011002, 0xE1811002, 0xE0211002,
            0xE1C11002, 0xE1510002, 0xE0511002, 0xE0911002, 0xE1B01002, 0xE1110002, 0xE1A01102,
            0xE1E01122, 0xE0811142, 0xE0411162, 0xE0611102, 0xE0011122, 0xE1811142, 0xE0211162,
            0xE1C11102, 0xE5910004, 0xE5D10004, 0xE5810004, 0xE5C10004, 0xE59F0004, 0xE1A0F00E,
            0xE12FFF1E, 0xEB000000, 0xEA000000,
        ];
        let mut seen = std::collections::BTreeSet::new();
        for w in words {
            let i = decode_instr(w);
            assert!(supported(i.op), "{w:08X} -> {:?}", i.op);
            seen.insert(i.op as u8);
            gen_block(&[i]);
        }
        // SPECIAL_OPS が特化の Op を順に全部並べていること（Op は repr(u8) で、特化は
        // MovImm から BSpin まで連続）。
        for (n, op) in SPECIAL_OPS.iter().enumerate() {
            assert_eq!(*op as u8, Op::MovImm as u8 + n as u8);
        }
        assert_eq!(*SPECIAL_OPS.last().unwrap(), Op::BSpin);
        let all: Vec<u8> = SPECIAL_OPS
            .iter()
            .filter(|&&o| supported(o))
            .map(|&o| o as u8)
            .collect();
        assert_eq!(seen.into_iter().collect::<Vec<_>>(), all);
    }
}
