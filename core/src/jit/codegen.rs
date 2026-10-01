//! ブロックの切り出しと、IR → wasm 関数の生成（段階5。設計は tmp/internal-docs/docs/stage5-design.md）。
//!
//! 生成関数の約束（`(ctx) -> サイド出口か`。関数は物理ページ 1 枚につき 1 つで、
//! そのページのコンパイル済みブロックをすべて含む。段階5-2）:
//!   - 入口のブロックは regs[15] から決まる（ページ内の命令番号で br_table）。
//!   - 各ブロックの入口で「残りの命令数 ≥ ブロック長」を確かめ、足りなければ戻る
//!     （サイド出口。次の命令はインタプリタが実行する）。
//!   - 命令は IR（ir.rs）の意味をそのまま写す。1 IR 命令 = 1 ARM 命令。条件不成立の
//!     命令も 1 命令と数える（インタプリタと同じ）。
//!   - ロード・ストアはソフト TLB のヒット（権限あり・RAM・ストアなら wram あり）の
//!     ときだけ行う。それ以外はその命令の手前で戻る（サイド出口）。インタプリタが
//!     その命令から続けるので、MMIO・TLB の埋め方・フォルト・コードページへの
//!     書き込みの検出はインタプリタの経路で起きる。TLB は読むだけで変えない。
//!   - ブロックを終えたら、次の PC が同じ仮想ページにあり、その命令番号が関数内の
//!     ブロックの先頭なら、Rust に戻らずに続けて実行する（ブロックの連結）。
//!     Thumb に切り替わった・ページを出た・ブロックの先頭でない場合は戻る。
//!   - 戻るときは書き換えたレジスタと CPSR を書き戻し、regs[15] に次の PC を、
//!     ctx.executed に実行した命令数を入れ、サイド出口なら RET_SIDE を返す。
//!     でなければ、次の PC が別の関数のブロックの先頭なら ctx.next にその関数を入れて
//!     RET_LINK（Rust がすぐ続けて呼ぶ。段階5-3）、それ以外は RET_NORMAL を返す。
//!
//! 同じ仮想ページの中を続けて実行してよい理由: 生成コードは MMU の状態・ページの
//! 中身（コードページへのストアは出口）・割り込み線・期限を変えないので、Rust が
//! 入口で確かめた条件（code_cur_va が有効・割り込みなし・ARM 状態）は、Thumb への
//! BX を除いて関数の中で変わらない。上限はブロックごとに確かめる。
//!
//! PC はページの仮想アドレス（入口の regs[15] のページ）を基準にした定数の差分で
//! 扱う（同じ物理ページが別の仮想アドレスに見えていても、同じ生成コードが正しく
//! 動くように）。

use super::wasm::{Func, Local};
use super::{COMPILED, JitCtx, RET_LINK, RET_NORMAL, RET_SIDE};
use crate::arm::{FLAG_T, Instr, Op};
use crate::arm::{OP_ADC, OP_ADD, OP_AND, OP_BIC, OP_CMN, OP_CMP, OP_EOR, OP_MOV, OP_MVN};
use crate::arm::{OP_ORR, OP_RSB, OP_RSC, OP_SBC, OP_SUB, OP_TEQ, OP_TST};
use crate::arm::{VPAGE_ESZ, VPAGE_GEN, VPAGE_MASK, VPAGE_PAGE, VPAGE_VA};
use crate::mmu::{NO_RAM, TLB_SIZE, TLB_VALID, TlbEntry};

/// ブロックの最大の長さ（命令数）。生成関数を小さく保つため。
pub(crate) const MAX_BLOCK: usize = 64;

/// JIT する Op（段階5-1: 特化済みの単純なもの、5-2: ライトバック・ハーフワード・
/// LDM/STM・汎用のデータ処理を追加）。汎用の Op は命令語の形でさらに絞る（supported）。
pub(crate) const JIT_OPS: &[Op] = &[
    Op::DataProc,
    Op::Ldst,
    Op::LdstMisc,
    Op::LdmStm,
    Op::MulSwp,
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
];

/// JIT する命令か。
#[inline(always)]
pub(crate) fn supported(i: &Instr) -> bool {
    use Op::*;
    match i.op {
        DataProc => dp_ok(i.imm),
        LdmStm => ldm_ok(i.imm),
        Ldst => ldst_ok(i.imm),
        LdstMisc => ldst_misc_ok(i.imm),
        MulSwp => mul_ok(i.imm),
        // 汎用の残りと、BSpin（アイドルスキップの印を付ける）は対象外
        Mrs | Msr | Branch | Bx | Swi | McrMrc | UnimplV5 | UnimplMrsImm | UndefLdstReg
        | UndefLdcStc | UndefCdp | BSpin => false,
        _ => true,
    }
}

/// 汎用のデータ処理のうち JIT するもの: Rd が PC でない形（Rd=PC は分岐・例外復帰に
/// なるのでインタプリタに回す）。PC を読む形は read_reg と同じく PC+8 として扱う。
fn dp_ok(w: u32) -> bool {
    (w >> 12) & 0xF != 15
}

/// 汎用の LDR/STR/LDRB/STRB のうち JIT するもの: Rd が PC でなく、PC に書き戻さず、
/// LDRT/STRT（P=0・W=1）でない形。PC を読む形は PC+8。
fn ldst_ok(w: u32) -> bool {
    let wb = w & (1 << 24) == 0 || w & (1 << 21) != 0;
    (w >> 12) & 0xF != 15
        && !(wb && (w >> 16) & 0xF == 15)
        && !(w & (1 << 24) == 0 && w & (1 << 21) != 0)
}

/// 汎用のハーフワード・符号付き転送のうち JIT するもの: Rd が PC でなく、PC に
/// 書き戻さず、LDRD/STRD（L=0 の SB/SH。v5TE）でない形。PC を読む形は PC+8。
fn ldst_misc_ok(w: u32) -> bool {
    let (load, sh) = (w & (1 << 20) != 0, (w >> 5) & 3);
    let wb = w & (1 << 24) == 0 || w & (1 << 21) != 0;
    (w >> 12) & 0xF != 15 && !(wb && (w >> 16) & 0xF == 15) && (load || sh == 1) && sh != 0
}

/// 乗算のうち JIT するもの: MUL/MLA/UMULL/UMLAL/SMULL/SMLAL で PC を使わない形
/// （SWP は対象外）。
fn mul_ok(w: u32) -> bool {
    w & (1 << 24) == 0
        && [w & 0xF, (w >> 8) & 0xF, (w >> 12) & 0xF, (w >> 16) & 0xF]
            .iter()
            .all(|&r| r != 15)
}

/// LDM/STM のうち JIT するもの: S=0（ユーザーバンク転送・CPSR の復帰でない）、
/// Rn が PC でない、リストが空でない形。
fn ldm_ok(w: u32) -> bool {
    w & (1 << 22) == 0 && (w >> 16) & 0xF != 15 && w & 0xFFFF != 0
}

/// PC を書く（ブロックを終える）命令。
pub(crate) fn terminates(i: &Instr) -> bool {
    match i.op {
        Op::B | Op::Bl | Op::BxReg | Op::MovPcReg => true,
        // PC を含む LDM
        Op::LdmStm => i.imm & (1 << 20) != 0 && i.imm & (1 << 15) != 0,
        _ => false,
    }
}

/// ページ内の命令 idx（0〜1023）から始まるブロックを切り出す。instr_at は
/// ページ内の命令番号の命令を返す。対象外の命令・PC を書く命令（含める）・
/// ページ末尾・MAX_BLOCK で終わる。先頭が対象外なら空。
/// is_entry(i) が真の命令番号（関数内の別のブロックの先頭）の手前でも終える
/// （そこからは関数の中の振り分けで続ける。同じ命令を複数のブロックに重ねて
/// 生成しないため。重ねると関数が大きくなり、V8 がコンパイルでメモリを使い果たした）。
pub(crate) fn form_block(
    idx: u32,
    mut instr_at: impl FnMut(u32) -> Instr,
    is_entry: impl Fn(u32) -> bool,
) -> Vec<Instr> {
    let mut v = vec![];
    let mut i = idx;
    while i < 1024 && v.len() < MAX_BLOCK && (i == idx || !is_entry(i)) {
        let ins = instr_at(i);
        if !supported(&ins) {
            break;
        }
        v.push(ins);
        if terminates(&ins) {
            break;
        }
        i += 1;
    }
    v
}

/// 命令番号 s から始まるブロック（form_block の結果、空でない）の後に、同じページで
/// 実行が続き得る命令番号（ページの外・動的な分岐先は含まない）。
pub(crate) fn successors(s: u32, block: &[Instr]) -> Vec<u32> {
    let j = s + block.len() as u32 - 1;
    let last = &block[block.len() - 1];
    let mut v = vec![];
    if terminates(last) {
        if matches!(last.op, Op::B | Op::Bl) {
            // 分岐先のページ内のオフセット（imm は regs[15] = PC+4 に足す差分）
            let o = (4 * j + 4).wrapping_add(last.imm);
            if o < 0x1000 {
                v.push(o / 4);
            }
        }
        if last.cond != 0xE || last.op == Op::Bl {
            v.push(j + 1); // 不成立の続き・BL からの戻り先
        }
    } else {
        // 対象外の命令・MAX_BLOCK・ページ末尾で切れた。対象外の命令はインタプリタが
        // 実行し、その次がブロックの先頭になる。
        v.push(j + 1);
        v.push(j + 2);
    }
    v
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
/// 次に実行する（出口では regs[15] に入れる）PC
const NPC: Local = 9;
/// 実行中のページの仮想アドレスの先頭
const PAGE: Local = 10;
const T0: Local = 11;
const T1: Local = 12;
const T2: Local = 13;
const T3: Local = 14;
/// 上限までの残りの命令数・実行した命令数・サイド出口の印
const REM: Local = 15;
const EXEC: Local = 16;
const SIDEF: Local = 17;
const T4: Local = 18;
const T5: Local = 19;
const T6: Local = 20;
const T7: Local = 21;
/// サイド出口の位置（命令番号 | ブロック内の実行済み命令数 << 10。共通の出口が読む）
const EXK: Local = 22;
/// r0〜r14（r15 はローカルに持たない。読みは PAGE からの定数の差分）
const R0: Local = 23;
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
const CTX_EXECUTED: u32 = std::mem::offset_of!(JitCtx, executed) as u32;
const CTX_VPAGES: u32 = std::mem::offset_of!(JitCtx, vpages) as u32;
const CTX_JIT_TAB: u32 = std::mem::offset_of!(JitCtx, jit_tab) as u32;
const CTX_GEN_LO: u32 = std::mem::offset_of!(JitCtx, gen_lo) as u32;
const CTX_LINK_OK: u32 = std::mem::offset_of!(JitCtx, link_ok) as u32;
const CTX_NEXT: u32 = std::mem::offset_of!(JitCtx, next) as u32;
// 世代は gen_lo・gen_hi を続けて 1 つの i64（リトルエンディアン）として読む。
const _: () =
    assert!(std::mem::offset_of!(JitCtx, gen_hi) == std::mem::offset_of!(JitCtx, gen_lo) + 4);
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
        LdrImm | LdrbImm | LdrhImm | LdrsbImm | LdrshImm => (rn, rd),
        StrImm | StrbImm | StrhImm => (rn | rd, 0),
        LdrPre | LdrPost | LdrbPre | LdrbPost => (rn, rd | rn),
        StrPre | StrPost | StrbPre | StrbPost => (rn | rd, rn),
        Bl => (0, bit(14)),
        DataProc => {
            let w = i.imm;
            let op = (w >> 21) & 0xF;
            let mut rd_ = if (OP_TST..=OP_CMN).contains(&op) {
                0
            } else {
                bit((w >> 12) & 0xF)
            };
            let mut r = if op != OP_MOV && op != OP_MVN {
                bit((w >> 16) & 0xF)
            } else {
                0
            };
            if w & (1 << 25) == 0 {
                r |= bit(w & 0xF);
                if w & 0x10 != 0 {
                    r |= bit((w >> 8) & 0xF);
                }
            }
            if rd_ == 0 {
                rd_ = 0;
            }
            (r, rd_)
        }
        Ldst => {
            let w = i.imm;
            let (rn_, rd_) = (bit((w >> 16) & 0xF), bit((w >> 12) & 0xF));
            let rm_ = if w & (1 << 25) != 0 { bit(w & 0xF) } else { 0 };
            let wb = if w & (1 << 24) == 0 || w & (1 << 21) != 0 {
                rn_
            } else {
                0
            };
            if w & (1 << 20) != 0 {
                (rn_ | rm_, rd_ | wb)
            } else {
                (rn_ | rm_ | rd_, wb)
            }
        }
        LdstMisc => {
            let w = i.imm;
            let (rn_, rd_) = (bit((w >> 16) & 0xF), bit((w >> 12) & 0xF));
            let rm_ = if w & (1 << 22) == 0 { bit(w & 0xF) } else { 0 };
            let wb = if w & (1 << 24) == 0 || w & (1 << 21) != 0 {
                rn_
            } else {
                0
            };
            if w & (1 << 20) != 0 {
                (rn_ | rm_, rd_ | wb)
            } else {
                (rn_ | rm_ | rd_, wb)
            }
        }
        MulSwp => {
            let w = i.imm;
            let (rm_, rs_) = (bit(w & 0xF), bit((w >> 8) & 0xF));
            let (r12, r16) = (bit((w >> 12) & 0xF), bit((w >> 16) & 0xF));
            let acc = w & (1 << 21) != 0;
            if w & (1 << 23) != 0 {
                // 長い乗算: RdLo = bits 15:12、RdHi = bits 19:16
                (rm_ | rs_ | if acc { r12 | r16 } else { 0 }, r12 | r16)
            } else {
                (rm_ | rs_ | if acc { r12 } else { 0 }, r16)
            }
        }
        LdmStm => {
            let w = i.imm;
            let (rn_, list) = (bit((w >> 16) & 0xF), (w & 0x7FFF) as u16);
            let wb = if w & (1 << 21) != 0 { rn_ } else { 0 };
            if w & (1 << 20) != 0 {
                (rn_, list | wb)
            } else {
                (rn_ | list, wb)
            }
        }
        _ => (0, 0),
    }
}

/// CPSR を読み書きするか（条件・フラグ・BX の T）。
fn uses_cpsr(i: &Instr) -> bool {
    // 汎用のデータ処理は C を読むことがある（ADC 等・RRX・シフタキャリー）
    i.cond != 0xE
        || sets_cpsr(i)
        || i.op == Op::DataProc
        || (i.op == Op::Ldst && i.imm & (1 << 25) != 0) // RRX のオフセット
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
    ) || (matches!(i.op, DataProc | MulSwp) && i.imm & (1 << 20) != 0)
}

fn is_mem(op: Op) -> bool {
    use Op::*;
    matches!(
        op,
        LdrImm
            | LdrbImm
            | StrImm
            | StrbImm
            | LdrhImm
            | LdrsbImm
            | LdrshImm
            | StrhImm
            | LdrPre
            | LdrPost
            | LdrbPre
            | LdrbPost
            | StrPre
            | StrPost
            | StrbPre
            | StrbPost
            | LdmStm
            | Ldst
            | LdstMisc
    )
}

/// 補助関数の番号（モジュールの中の関数番号。wasm::module に helpers() の順で渡す）。
const HELPER_LOAD: u32 = 0;
const HELPER_STORE: u32 = 1;

/// 生成関数から呼ぶ補助関数（(引数の数, 関数)）。どのモジュールにも同じものを入れる。
///
/// TLB を引く `(va, tlb, pid, perm, arena) -> アリーナ上のアドレス（外れなら 0）`。
/// 読み（ram）と書き（wram）の 2 つ。MMU の fast path と同じ条件でヒットを判定し、
/// TLB は読むだけ。アリーナは Rust のヒープ上にあるので、ヒットのアドレスは 0 に
/// ならない。
pub(crate) fn helpers() -> Vec<(u32, Func)> {
    const VA: Local = 0;
    const TLB_P: Local = 1;
    const PID_P: Local = 2;
    const PERM_P: Local = 3;
    const ARENA_P: Local = 4;
    const MVA: Local = 5;
    const E: Local = 6;
    let tlb = |write: bool| {
        let mut f = Func::new(2);
        // MVA（FCSE: VA < 32MB は PID を OR する）
        f.get(VA)
            .get(PID_P)
            .or()
            .get(VA)
            .get(VA)
            .i32(0x0200_0000)
            .lt_u()
            .select()
            .set(MVA);
        // エントリ（直接マップ、添字は MVA[21:12]）
        f.get(MVA)
            .i32(12)
            .shr_u()
            .i32(TLB_SIZE as u32 - 1)
            .and()
            .i32(TLB_ESZ)
            .mul()
            .get(TLB_P)
            .add()
            .tee(E);
        f.load(TLB_TAG)
            .get(MVA)
            .i32(12)
            .shr_u()
            .i32(TLB_VALID)
            .or()
            .ne()
            .if_()
            .i32(0)
            .ret()
            .end();
        f.get(E)
            .load8_u(TLB_PERM)
            .get(PERM_P)
            .and()
            .eqz()
            .if_()
            .i32(0)
            .ret()
            .end();
        f.get(E)
            .load(if write { TLB_WRAM } else { TLB_RAM })
            .tee(E)
            .i32(NO_RAM)
            .eq()
            .if_()
            .i32(0)
            .ret()
            .end();
        f.get(ARENA_P).get(E).add().get(VA).i32(0xFFF).and().add();
        (5, f)
    };
    vec![tlb(false), tlb(true)]
}

struct Gen {
    f: Func,
    /// 実行中のブロックの先頭の命令番号
    base: u32,
    /// 出口（$exit）までに囲んでいるブロックの数（ブロックの中の if を含む）
    depth: u32,
    /// 生成中の命令が汎用の Op か（汎用の実行関数は r15 を read_reg の PC+8 で読む。
    /// 特化した Op は regs[15] = PC+4 のまま読み、必要なら即値で補正済み）
    pc8: bool,
}

/// 物理ページ 1 枚のブロック（(先頭の命令番号, form_block の結果)。空でなく、先頭の
/// 命令番号が重ならないもの）を 1 つの wasm 関数にする。
pub(crate) fn gen_page(blocks: &[(u32, Vec<Instr>)]) -> Func {
    let (mut used, mut written) = (0u16, 0u16);
    let (mut cpsr, mut cpsr_set, mut mem) = (false, false, false);
    for (_, b) in blocks {
        for i in b {
            let (rd, wr) = reads_writes(i);
            used |= rd | wr;
            written |= wr;
            cpsr |= uses_cpsr(i);
            cpsr_set |= sets_cpsr(i);
            mem |= is_mem(i.op);
        }
    }
    let n = blocks.len() as u32;
    let mut g = Gen {
        f: Func::new(NUM_LOCALS),
        base: 0,
        depth: 0,
        pc8: false,
    };
    let f = &mut g.f;
    f.get(CTX).load(CTX_REMAINING).set(REM);
    f.get(CTX).load(CTX_REGS).tee(REGS).load(60).tee(NPC);
    f.i32(!0xFFF).and().set(PAGE);
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
    for x in 0..15u8 {
        if used & (1 << x) != 0 {
            f.get(REGS).load(4 * x as u32).set(r(x));
        }
    }
    // loop $L { block $exit { block $side { block $c(n-1) … block $c0 { br_table }
    // ブロック 0 … ブロック n-1 } 共通のサイド出口 } 出口 }。ブロック k は $c(k) の
    // end の直後に置く。
    f.loop_();
    f.block();
    f.block();
    for _ in 0..n {
        f.block();
    }
    // 命令番号 → ブロック（ブロックの先頭でなければ $exit）。表は関数内の先頭の
    // 命令番号の範囲だけにする（範囲外は引き算で大きな値になり既定の $exit へ）。
    let lo = blocks.iter().map(|b| b.0).min().unwrap_or(0);
    let hi = blocks.iter().map(|b| b.0).max().unwrap_or(0);
    let mut table = vec![n + 1; (hi - lo + 1) as usize];
    for (k, (idx, _)) in blocks.iter().enumerate() {
        table[(*idx - lo) as usize] = k as u32;
    }
    f.get(NPC).i32(2).shr_u().i32(0x3FF).and();
    if lo != 0 {
        f.i32(lo).sub();
    }
    f.br_table(&table, n + 1);
    for (k, (idx, b)) in blocks.iter().enumerate() {
        g.f.end();
        g.base = *idx;
        // ここで $exit までに囲むのは $c(k+1)…$c(n-1) と $side
        g.depth = n - k as u32;
        g.block(b);
    }
    g.f.end(); // $side
    // 共通のサイド出口（各所は EXK を入れて $side へ分岐する。出口の命令列を
    // 各所に展開すると関数が大きくなり、V8 の最適化コンパイルの作業領域が増える）
    let f = &mut g.f;
    f.get(PAGE)
        .get(EXK)
        .i32(0x3FF)
        .and()
        .i32(2)
        .shl()
        .add()
        .set(NPC);
    f.get(EXEC).get(EXK).i32(10).shr_u().add().set(EXEC);
    f.i32(1).set(SIDEF);
    g.f.end(); // $exit
    // 出口: 書き戻して戻る
    let f = &mut g.f;
    for x in 0..15u8 {
        if written & (1 << x) != 0 {
            f.get(REGS).get(r(x)).store(4 * x as u32);
        }
    }
    if cpsr_set {
        f.get(CPSR_A).get(CPSR).store(0);
    }
    f.get(REGS).get(NPC).store(60);
    f.get(CTX).get(EXEC).store(CTX_EXECUTED);
    f.get(SIDEF).if_().i32(RET_SIDE).ret().end();
    // 連結（段階5-3）: 次の PC が別の関数のブロックの先頭なら、その関数の番号を
    // ctx.next に入れて RET_LINK を返す。引き当ては CodeCache::enter の vpages が
    // 当たる場合と同じ判定（仮想ページと変換の世代が一致）で、枠が COMPILED の
    // ときだけ。書き込みで捨てるページが溜まっていれば（link_ok = 0）連結しない
    // （enter はそれを先に捨てるので）。Thumb に切り替わったときもしない。
    f.get(CTX).load(CTX_LINK_OK);
    f.get(CTX).load(CTX_CPSR).load(0).i32(FLAG_T).and().eqz();
    f.and().if_();
    f.get(NPC)
        .i32(12)
        .shr_u()
        .i32(VPAGE_MASK)
        .and()
        .i32(VPAGE_ESZ)
        .mul()
        .get(CTX)
        .load(CTX_VPAGES)
        .add()
        .tee(T0);
    f.load(VPAGE_VA).get(NPC).i32(!0xFFF).and().eq();
    f.get(T0)
        .i64_load(VPAGE_GEN)
        .get(CTX)
        .i64_load(CTX_GEN_LO)
        .i64_eq();
    f.and().if_();
    // 枠の表の要素（None は 0）
    f.get(T0)
        .load(VPAGE_PAGE)
        .i32(4)
        .mul()
        .get(CTX)
        .load(CTX_JIT_TAB)
        .add()
        .load(0)
        .tee(T1)
        .if_();
    f.get(T1)
        .get(NPC)
        .i32(2)
        .shr_u()
        .i32(0x3FF)
        .and()
        .i32(4)
        .mul()
        .add()
        .load(0)
        .tee(T2)
        .i32(COMPILED)
        .and()
        .if_();
    f.get(CTX).get(T2).i32(!COMPILED).and().store(CTX_NEXT);
    f.i32(RET_LINK).ret();
    f.end().end().end().end();
    f.i32(RET_NORMAL).ret();
    f.end(); // $L（ここには来ないが、関数の型に合わせて値を置く）
    f.i32(0);
    g.f
}

/// ブロック 1 つ（form_block の結果、空でない）を生成したときの命令列の大きさ
/// （関数の入口・振り分け・出口を除く。関数を上限の大きさで分ける目安）。
pub(crate) fn block_size(idx: u32, b: &[Instr]) -> usize {
    let mut g = Gen {
        f: Func::new(NUM_LOCALS),
        base: idx,
        depth: 1,
        pc8: false,
    };
    g.block(b);
    g.f.len()
}

impl Gen {
    /// ブロックの k 命令目の PC を積む。
    fn pc(&mut self, k: u32) {
        self.f.get(PAGE);
        let off = 4 * (self.base + k);
        if off != 0 {
            self.f.i32(off).add();
        }
    }

    /// レジスタ n の値を積む（r15 は、特化した Op では実行中の regs[15] = PC+4、
    /// 汎用の Op では read_reg の PC+8。どちらも IR の実行と同じ）。
    fn reg(&mut self, k: u32, n: u8) {
        if n == 15 {
            self.pc(k + if self.pc8 { 2 } else { 1 });
        } else {
            self.f.get(r(n));
        }
    }

    /// $exit へ分岐する（depth は今囲んでいる if の数を含む）。
    fn br_exit(&mut self) {
        self.f.br(self.depth);
    }

    /// サイド出口: ブロックの k 命令目の手前で戻る（k 命令は実行済み）。共通の出口
    /// （$side。$exit の 1 つ内側）で NPC・EXEC・SIDEF を入れる。
    fn side_exit(&mut self, k: u32) {
        self.f.i32((self.base + k) | (k << 10)).set(EXK);
        self.f.br(self.depth - 1);
    }

    /// if を開く（side_exit の深さに数える）。
    fn open_if(&mut self) {
        self.f.if_();
        self.depth += 1;
    }

    fn close_if(&mut self) {
        self.f.end();
        self.depth -= 1;
    }

    /// ブロック 1 つ（入口の上限の確認、命令列、次のブロックへの連結）。
    fn block(&mut self, b: &[Instr]) {
        let len = b.len() as u32;
        // 上限の確認（ブロックの途中では数えない）。NPC は入口の PC のまま。
        self.f.get(REM).i32(len).lt_u();
        self.open_if();
        self.side_exit(0);
        self.close_if();
        let mut term = None;
        for (k, i) in b.iter().enumerate() {
            let k = k as u32;
            if terminates(i) {
                // 条件不成立なら次の命令へ。
                self.pc(k + 1);
                self.f.set(NPC);
                term = Some(i.op);
            }
            let cond = i.cond != 0xE;
            if cond {
                self.cond(i.cond);
                self.open_if();
            }
            self.instr(k, i);
            if cond {
                self.close_if();
            }
        }
        if term.is_none() {
            self.pc(len);
            self.f.set(NPC);
        }
        let f = &mut self.f;
        f.get(EXEC).i32(len).add().set(EXEC);
        f.get(REM).i32(len).sub().set(REM);
        if term == Some(Op::BxReg) {
            // Thumb に切り替わったら戻る（Rust が Thumb の実行に回す）。
            self.f.get(CPSR).i32(FLAG_T).and();
            self.open_if();
            self.br_exit();
            self.close_if();
        }
        // 同じ仮想ページなら続ける（$L は $exit の 1 つ外）。
        let f = &mut self.f;
        f.get(NPC).get(PAGE).xor().i32(!0xFFF).and().eqz();
        f.br_if(self.depth + 1);
        self.br_exit();
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
    /// 引くのは補助関数（helpers。アクセスごとに展開すると関数が大きくなるため）。
    fn mem_addr(&mut self, k: u32, write: bool) {
        let f = &mut self.f;
        f.get(T1).get(TLB).get(PID);
        f.get(if write { PERM_W } else { PERM_R }).get(ARENA);
        f.call(if write { HELPER_STORE } else { HELPER_LOAD });
        f.tee(T3).eqz();
        self.open_if();
        self.side_exit(k);
        self.close_if();
        self.f.get(T3);
    }

    /// LDM/STM（ldm_ok の形）。exec_ldm_stm の「1 ページ内・TLB ヒット」の経路と同じ
    /// 条件のときだけ実行し、それ以外（ページをまたぐ・TLB ミス・MMIO・コードページへの
    /// ストア）は出口にする（インタプリタが 1 ワードずつの経路とアボートを扱う）。
    fn ldm_stm(&mut self, k: u32, w: u32) {
        let (pre, up, wb, load) = (
            w & (1 << 24) != 0,
            w & (1 << 23) != 0,
            w & (1 << 21) != 0,
            w & (1 << 20) != 0,
        );
        let rn = ((w >> 16) & 0xF) as u8;
        let list = w & 0xFFFF;
        let n = list.count_ones();
        let delta = match (up, pre) {
            (true, false) => 0,
            (true, true) => 4,
            (false, false) => (4u32).wrapping_sub(4 * n),
            (false, true) => (4 * n).wrapping_neg(),
        };
        let new_base = if up { 4 * n } else { (4 * n).wrapping_neg() };
        self.reg(k, rn);
        self.f.tee(T0).i32(delta).add().i32(!3).and().tee(T1);
        // 1 ページに収まらなければ出口（ram_run と同じ条件）
        self.f.i32(0xFFF).and().i32(0x1000 - 4 * n).gt_u();
        self.open_if();
        self.side_exit(k);
        self.close_if();
        self.mem_addr(k, !load);
        self.f.set(T5);
        let regs = (0..16u8).filter(|&x| list & (1 << x) != 0);
        if load {
            // ライトバックが先。Rn がリストにあればロード値が上書きする。
            if wb {
                self.f.get(T0).i32(new_base).add().set(r(rn));
            }
            for (j, x) in regs.enumerate() {
                self.f.get(T5).load(4 * j as u32);
                if x == 15 {
                    self.f.i32(!3).and().set(NPC); // load_pc（S=0）
                } else {
                    self.f.set(r(x));
                }
            }
        } else {
            // 全部書いてからライトバック（Rn は変更前の値を書く）。PC は PC+8。
            for (j, x) in regs.enumerate() {
                self.f.get(T5);
                if x == 15 {
                    self.pc(k + 2);
                } else {
                    self.f.get(r(x));
                }
                self.f.store(4 * j as u32);
            }
            if wb {
                self.f.get(T0).i32(new_base).add().set(r(rn));
            }
        }
    }

    /// 汎用の LDR/STR/LDRB/STRB（ldst_ok の形。exec_ldst と同じ意味・順序）。
    fn ldst(&mut self, k: u32, w: u32) {
        let (pre, up, byte, wb, load) = (
            w & (1 << 24) != 0,
            w & (1 << 23) != 0,
            w & (1 << 22) != 0,
            w & (1 << 21) != 0,
            w & (1 << 20) != 0,
        );
        let (rn, rd) = (((w >> 16) & 0xF) as u8, ((w >> 12) & 0xF) as u8);
        // T2 = オフセット（スケーリング付きレジスタは即値シフトの値だけ）
        if w & (1 << 25) != 0 {
            self.reg_shift_imm(k, w, false);
        } else {
            self.f.i32(w & 0xFFF).set(T2);
        }
        self.xfer(
            k,
            rn,
            rd,
            pre,
            up,
            !pre || wb,
            load,
            if byte { 1 } else { 4 },
            false,
        );
    }

    /// 汎用のハーフワード・符号付き転送（ldst_misc_ok の形。exec_ldst_misc と同じ）。
    fn ldst_misc(&mut self, k: u32, w: u32) {
        let (pre, up, wb, load) = (
            w & (1 << 24) != 0,
            w & (1 << 23) != 0,
            w & (1 << 21) != 0,
            w & (1 << 20) != 0,
        );
        let (rn, rd) = (((w >> 16) & 0xF) as u8, ((w >> 12) & 0xF) as u8);
        if w & (1 << 22) != 0 {
            self.f.i32(((w >> 4) & 0xF0) | (w & 0xF)).set(T2);
        } else {
            self.reg(k, (w & 0xF) as u8);
            self.f.set(T2);
        }
        // sh: 01 = H（ゼロ拡張）、10 = SB、11 = SH
        let (size, signed) = match (w >> 5) & 3 {
            1 => (2, false),
            2 => (1, true),
            _ => (2, true),
        };
        self.xfer(k, rn, rd, pre, up, !pre || wb, load, size, signed);
    }

    /// 転送の本体（オフセットは T2）。size は 1/2/4、signed は LDRSB（size=1）・LDRSH。
    #[allow(clippy::too_many_arguments)]
    fn xfer(
        &mut self,
        k: u32,
        rn: u8,
        rd: u8,
        pre: bool,
        up: bool,
        writeback: bool,
        load: bool,
        size: u32,
        signed: bool,
    ) {
        self.reg(k, rn);
        self.f.tee(T0).get(T2);
        if up {
            self.f.add();
        } else {
            self.f.sub();
        }
        self.f.set(T4); // indexed
        if pre {
            self.f.get(T4).set(T0);
        }
        // T0 = アクセスするアドレス（ワード・ハーフワードは揃えてから TLB を引く）
        self.f.get(T0);
        match size {
            4 => {
                self.f.i32(!3).and();
            }
            2 => {
                self.f.i32(!1).and();
            }
            _ => {}
        }
        self.f.set(T1);
        self.mem_addr(k, !load);
        if load {
            match (size, signed) {
                (4, _) => {
                    self.f.load(0).get(T0).i32(3).and().i32(3).shl().rotr();
                }
                (2, false) => {
                    self.f.load16_u(0);
                }
                (2, true) => {
                    self.f.load16_s(0);
                }
                (_, false) => {
                    self.f.load8_u(0);
                }
                _ => {
                    self.f.load8_s(0);
                }
            }
            self.f.set(T5);
            if writeback {
                self.f.get(T4).set(r(rn));
            }
            self.f.get(T5).set(r(rd));
        } else {
            self.reg(k, rd);
            match size {
                4 => self.f.store(0),
                2 => self.f.store16(0),
                _ => self.f.store8(0),
            };
            if writeback {
                self.f.get(T4).set(r(rn));
            }
        }
    }

    /// MUL/MLA/UMULL/UMLAL/SMULL/SMLAL（mul_ok の形。exec_mul・exec_mul_long と同じ）。
    fn mul(&mut self, k: u32, w: u32) {
        let (rm, rs) = ((w & 0xF) as u8, ((w >> 8) & 0xF) as u8);
        let (r12, r16) = (((w >> 12) & 0xF) as u8, ((w >> 16) & 0xF) as u8);
        let (acc, s) = (w & (1 << 21) != 0, w & (1 << 20) != 0);
        if w & (1 << 23) == 0 {
            self.reg(k, rm);
            self.reg(k, rs);
            self.f.mul();
            if acc {
                self.reg(k, r12);
                self.f.add();
            }
            self.f.set(T1);
            self.f.get(T1).set(r(r16));
            if s {
                self.set_nz(T1, 0x3FFF_FFFF, 0);
            }
            return;
        }
        let signed = w & (1 << 22) != 0;
        // 64 ビットの積（と加算）をスタックで 2 回計算して下位・上位を取り出す
        // （i64 のローカルを持たないため）。
        let prod = |g: &mut Gen| {
            g.reg(k, rm);
            if signed {
                g.f.i64_extend_s()
            } else {
                g.f.i64_extend_u()
            };
            g.reg(k, rs);
            if signed {
                g.f.i64_extend_s()
            } else {
                g.f.i64_extend_u()
            };
            g.f.i64_mul();
            if acc {
                g.reg(k, r16);
                g.f.i64_extend_u().i64(32).i64_shl();
                g.reg(k, r12);
                g.f.i64_extend_u().i64_or().i64_add();
            }
        };
        prod(self);
        self.f.wrap().set(T1); // 下位
        prod(self);
        self.f.i64(32).i64_shr_u().wrap().set(T2); // 上位
        self.f.get(T1).set(r(r12));
        self.f.get(T2).set(r(r16));
        if s {
            // N = bit63、Z = 64 ビット全体が 0
            let f = &mut self.f;
            f.get(CPSR).i32(0x3FFF_FFFF).and();
            f.get(T2).i32(0x8000_0000).and().or();
            f.get(T1).get(T2).or().eqz().i32(30).shl().or();
            f.set(CPSR);
        }
    }

    /// シフタキャリー（C フラグの今の値）を 0/1 で積む。
    fn carry_in(&mut self) {
        self.f.get(CPSR).i32(29).shr_u().i32(1).and();
    }

    /// 汎用のデータ処理（dp_ok の形。exec_data_proc と同じ意味）。
    /// T2 = 第 2 オペランド、T3 = シフタキャリー（0/1）、T0 = Rn、T1 = 結果。
    fn data_proc(&mut self, k: u32, w: u32) {
        let op = (w >> 21) & 0xF;
        let s = w & (1 << 20) != 0;
        let (rn, rd) = (((w >> 16) & 0xF) as u8, ((w >> 12) & 0xF) as u8);
        let logical = matches!(
            op,
            OP_AND | OP_EOR | OP_TST | OP_TEQ | OP_ORR | OP_MOV | OP_BIC | OP_MVN
        );
        let need_c = s && logical;
        // ---- 第 2 オペランドとシフタキャリー ----
        if w & (1 << 25) != 0 {
            let rot = ((w >> 8) & 0xF) * 2;
            let val = (w & 0xFF).rotate_right(rot);
            self.f.i32(val).set(T2);
            if need_c {
                if rot == 0 {
                    self.carry_in();
                } else {
                    self.f.i32(val >> 31);
                }
                self.f.set(T3);
            }
        } else if w & 0x10 == 0 {
            self.reg_shift_imm(k, w, need_c);
        } else {
            self.reg_shift_reg(k, w);
        }
        // ---- 演算 ----
        if op != OP_MOV && op != OP_MVN {
            self.reg(k, rn);
            self.f.set(T0);
        }
        let f = &mut self.f;
        let arith = match op {
            OP_AND | OP_TST => {
                f.get(T0).get(T2).and();
                None
            }
            OP_EOR | OP_TEQ => {
                f.get(T0).get(T2).xor();
                None
            }
            OP_ORR => {
                f.get(T0).get(T2).or();
                None
            }
            OP_MOV => {
                f.get(T2);
                None
            }
            OP_BIC => {
                f.get(T0).get(T2).i32(u32::MAX).xor().and();
                None
            }
            OP_MVN => {
                f.get(T2).i32(u32::MAX).xor();
                None
            }
            // add_with_carry(a, b, cin) の (a, b の反転, cin): 0 = 定数 0、1 = 定数 1、2 = C
            OP_SUB | OP_CMP => Some((T0, T2, true, 1)),
            OP_RSB => Some((T2, T0, true, 1)),
            OP_ADD | OP_CMN => Some((T0, T2, false, 0)),
            OP_ADC => Some((T0, T2, false, 2)),
            OP_SBC => Some((T0, T2, true, 2)),
            _ => {
                debug_assert_eq!(op, OP_RSC);
                Some((T2, T0, true, 2))
            }
        };
        match arith {
            None => {
                self.f.set(T1);
                if s {
                    // N・Z は結果、C はシフタキャリー、V は不変
                    let f = &mut self.f;
                    f.get(CPSR).i32(0x1FFF_FFFF).and();
                    f.get(T1).i32(0x8000_0000).and().or();
                    f.get(T1).eqz().i32(30).shl().or();
                    f.get(T3).i32(29).shl().or();
                    f.set(CPSR);
                }
            }
            Some((a, b, inv, cin)) => {
                // T6 = a、T7 = b（反転済み）、T4 = a + b、T1 = T4 + cin
                let f = &mut self.f;
                f.get(a).set(T6);
                f.get(b);
                if inv {
                    f.i32(u32::MAX).xor();
                }
                f.set(T7);
                f.get(T6).get(T7).add().tee(T4);
                match cin {
                    0 => {}
                    1 => {
                        f.i32(1).add();
                    }
                    _ => {
                        self.carry_in();
                        self.f.add();
                    }
                }
                self.f.set(T1);
                if s {
                    let f = &mut self.f;
                    f.get(CPSR).i32(0x0FFF_FFFF).and();
                    f.get(T1).i32(0x8000_0000).and().or();
                    f.get(T1).eqz().i32(30).shl().or();
                    // C: a + b の桁上がり、または + cin の桁上がり
                    f.get(T4).get(T6).lt_u().get(T1).get(T4).lt_u().or();
                    f.i32(29).shl().or();
                    // V: (a ^ res) & (b ^ res) の最上位
                    f.get(T6).get(T1).xor().get(T7).get(T1).xor().and();
                    f.i32(31).shr_u().i32(28).shl().or();
                    f.set(CPSR);
                }
            }
        }
        if !(OP_TST..=OP_CMN).contains(&op) {
            self.f.get(T1).set(r(rd));
        }
    }

    /// 即値シフト（shift_imm）: T2 = 値、need_c なら T3 = キャリー。
    fn reg_shift_imm(&mut self, k: u32, w: u32, need_c: bool) {
        let (t, a) = ((w >> 5) & 3, (w >> 7) & 0x1F);
        self.reg(k, (w & 0xF) as u8);
        self.f.set(T5); // v
        let f = &mut self.f;
        match (t, a) {
            (0, 0) => {
                f.get(T5).set(T2);
            }
            (0, _) => {
                f.get(T5).i32(a).shl().set(T2);
            }
            (1, 0) => {
                f.i32(0).set(T2);
            }
            (1, _) => {
                f.get(T5).i32(a).shr_u().set(T2);
            }
            (2, 0) => {
                f.get(T5).i32(31).shr_s().set(T2);
            }
            (2, _) => {
                f.get(T5).i32(a).shr_s().set(T2);
            }
            (_, 0) => {
                // RRX
                f.get(T5).i32(1).shr_u();
                f.get(CPSR)
                    .i32(29)
                    .shr_u()
                    .i32(1)
                    .and()
                    .i32(31)
                    .shl()
                    .or()
                    .set(T2);
            }
            _ => {
                f.get(T5).i32(a).rotr().set(T2);
            }
        }
        if !need_c {
            return;
        }
        match (t, a) {
            (0, 0) => self.carry_in(),
            (0, _) => {
                self.f.get(T5).i32(32 - a).shr_u().i32(1).and();
            }
            (1, 0) | (2, 0) => {
                self.f.get(T5).i32(31).shr_u();
            }
            (1, _) | (2, _) => {
                self.f.get(T5).i32(a - 1).shr_u().i32(1).and();
            }
            (_, 0) => {
                self.f.get(T5).i32(1).and();
            }
            _ => {
                self.f.get(T2).i32(31).shr_u();
            }
        }
        self.f.set(T3);
    }

    /// レジスタ指定シフト（shift_reg）: T2 = 値、T3 = キャリー（量 0 なら今の C）。
    fn reg_shift_reg(&mut self, k: u32, w: u32) {
        let t = (w >> 5) & 3;
        self.reg(k, (w & 0xF) as u8);
        self.f.tee(T5).set(T2); // v（量 0 ならそのまま）
        self.reg(k, ((w >> 8) & 0xF) as u8);
        self.f.i32(0xFF).and().set(T4); // amount
        self.carry_in();
        self.f.set(T3);
        let f = &mut self.f;
        f.get(T4).if_(); // amount != 0
        match t {
            0 | 1 => {
                f.get(T4).i32(32).lt_u().if_();
                // 1〜31
                if t == 0 {
                    f.get(T5).i32(32).get(T4).sub().shr_u().i32(1).and().set(T3);
                    f.get(T5).get(T4).shl().set(T2);
                } else {
                    f.get(T5).get(T4).i32(1).sub().shr_u().i32(1).and().set(T3);
                    f.get(T5).get(T4).shr_u().set(T2);
                }
                f.else_();
                // 32 ならはみ出したビット、33 以上なら 0
                if t == 0 {
                    f.get(T5).i32(1).and();
                } else {
                    f.get(T5).i32(31).shr_u();
                }
                f.i32(0).get(T4).i32(32).eq().select().set(T3);
                f.i32(0).set(T2);
                f.end();
            }
            2 => {
                f.get(T4).i32(32).lt_u().if_();
                f.get(T5).get(T4).i32(1).sub().shr_u().i32(1).and().set(T3);
                f.get(T5).get(T4).shr_s().set(T2);
                f.else_();
                f.get(T5).i32(31).shr_u().set(T3);
                f.get(T5).i32(31).shr_s().set(T2);
                f.end();
            }
            _ => {
                // ROR: 量 & 31 が 0 なら値そのまま、C は bit31
                f.get(T5).get(T4).i32(31).and().rotr().tee(T2);
                f.i32(31).shr_u().set(T3);
            }
        }
        f.end();
    }

    /// 1 命令（条件は呼び出し側）。
    fn instr(&mut self, k: u32, i: &Instr) {
        use Op::*;
        self.pc8 = matches!(i.op, DataProc | Ldst | LdstMisc | MulSwp | LdmStm);
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
                let off = (4 * (self.base + k) + 4).wrapping_add(imm);
                self.f.get(PAGE).i32(off).add().set(NPC);
            }
            LdrhImm | LdrsbImm | LdrshImm | StrhImm => {
                self.reg(k, rn);
                self.f.i32(imm).add().tee(T0);
                if i.op != LdrsbImm {
                    self.f.i32(!1).and(); // 非アラインのハーフワードは揃える（汎用と同じ）
                }
                self.f.set(T1);
                self.mem_addr(k, i.op == StrhImm);
                match i.op {
                    LdrhImm => self.f.load16_u(0).set(r(rd)),
                    LdrsbImm => self.f.load8_s(0).set(r(rd)),
                    LdrshImm => self.f.load16_s(0).set(r(rd)),
                    _ => {
                        self.reg(k, rd);
                        self.f.store16(0)
                    }
                };
            }
            LdrPre | LdrPost | LdrbPre | LdrbPost | StrPre | StrPost | StrbPre | StrbPost => {
                // ldst_wb と同じ順序: アクセスが出口になったらベースは書き換えない。
                // ロードで Rd=Rn ならロード値が勝つ。ストアは書き換える前の Rd を書く。
                let load = matches!(i.op, LdrPre | LdrPost | LdrbPre | LdrbPost);
                let byte = matches!(i.op, LdrbPre | LdrbPost | StrbPre | StrbPost);
                let pre = matches!(i.op, LdrPre | LdrbPre | StrPre | StrbPre);
                self.reg(k, rn);
                self.f.tee(T0).i32(imm).add().set(T4); // T4 = 書き戻すベース
                if pre {
                    self.f.get(T4).set(T0); // T0 = アクセスするアドレス
                }
                self.f.get(T0);
                if !byte {
                    self.f.i32(!3).and();
                }
                self.f.set(T1);
                self.mem_addr(k, !load);
                if load {
                    if byte {
                        self.f.load8_u(0);
                    } else {
                        self.f.load(0).get(T0).i32(3).and().i32(3).shl().rotr();
                    }
                    self.f.set(T5);
                    self.f.get(T4).set(r(rn));
                    self.f.get(T5).set(r(rd));
                } else {
                    self.reg(k, rd);
                    if byte {
                        self.f.store8(0);
                    } else {
                        self.f.store(0);
                    }
                    self.f.get(T4).set(r(rn));
                }
            }
            LdmStm => self.ldm_stm(k, imm),
            Ldst => self.ldst(k, imm),
            LdstMisc => self.ldst_misc(k, imm),
            MulSwp => self.mul(k, imm),
            DataProc => self.data_proc(k, imm),
            _ => unreachable!("gen_page: unsupported op {:?}", i.op),
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
        let b = form_block(0, |i| decode_instr(words[i as usize]), |_| false);
        assert_eq!(b.len(), 3);
        // SWI は対象外
        let words = [0xE3A00001, 0xEF000000];
        let b = form_block(0, |i| decode_instr(words[i as usize]), |_| false);
        assert_eq!(b.len(), 1);
        let b = form_block(1, |i| decode_instr(words[i as usize]), |_| false);
        assert!(b.is_empty());
    }

    #[test]
    fn block_stops_at_page_end() {
        let b = form_block(1020, |_| decode_instr(0xE3A00001), |_| false);
        assert_eq!(b.len(), 4);
        let b = form_block(0, |_| decode_instr(0xE3A00001), |_| false);
        assert_eq!(b.len(), MAX_BLOCK);
        // 別のブロックの先頭の手前で終わる
        let b = form_block(0, |_| decode_instr(0xE3A00001), |i| i == 5);
        assert_eq!(b.len(), 5);
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
            0xE12FFF1E, 0xEB000000, 0xEA000000, // 5-1
            0xE1D100B2, 0xE1D100D2, 0xE1D100F2, 0xE1C100B2, 0xE5B10004, 0xE4910004, 0xE5F10004,
            0xE4D10004, 0xE5A10004, 0xE4810004, 0xE5E10004,
            0xE4C10004, // ハーフワード・ライトバック
            0xE0A12003, 0xE0B12312, 0xE1B01062, 0xE0F12353,
            0xE1D01000, // 汎用のデータ処理
            0xE8BD4010, 0xE92D4010, 0xE8BD8010, 0xE9900006, // LDM/STM
            0xE7912103, 0xE6112003, 0xE19120B3, 0xE11120D3, // 汎用の LDR/STR・LDRH 等
            0xE0020391, 0xE0221392, 0xE0821392, 0xE0E21392, // MUL/MLA/UMULL/SMLAL
        ];
        let mut seen = std::collections::BTreeSet::new();
        for w in words {
            let i = decode_instr(w);
            assert!(supported(&i), "{w:08X} -> {:?}", i.op);
            seen.insert(i.op as u8);
            gen_page(&[(0, vec![i])]);
            let _ = helpers();
        }
        let all: std::collections::BTreeSet<u8> = JIT_OPS.iter().map(|&o| o as u8).collect();
        assert_eq!(seen, all);
        // 表の先頭・対象外の形
        assert!(!supported(&decode_instr(0xE08FF002))); // ADD pc, pc, r2
        assert!(!supported(&decode_instr(0xE1B0F00E))); // MOVS pc, lr
        assert!(!supported(&decode_instr(0xE8DD8000))); // LDM ^ （S=1）
    }
}
