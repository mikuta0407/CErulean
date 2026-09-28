//! Go の cpu/arm のテスト（arm_test・thumb_test・exception_test・mul_test・
//! archregs_test）を移したもの。入力と期待値は Go と同じ。

// 表の行は Go のテーブルと同じ並びのタプルにしている。
#![allow(clippy::type_complexity)]

use super::exec_arm::*;
use super::*;

// ---- テスト用の System（Go の testMem・abortMem・fakeCP15）----

/// 64KB の平らなメモリ（アドレス 0 起点、MMU なし）と CP15 の記録。
/// bad を指すワードの 32 ビットアクセス（フェッチを含む）はアボートになる
/// （Go の abortMem: status=5 domain=2）。範囲外はバスエラー。
struct TestSys {
    mem: Vec<u8>,
    bad: Option<u32>,
    fsr: u32,
    far: u32,
    vec_base: u32,
    privileged: bool,
    irq: bool,
    fiq: bool,
    run: RunCtl,
}

impl TestSys {
    fn new() -> TestSys {
        TestSys {
            mem: vec![0; 64 * 1024],
            bad: None,
            fsr: 0,
            far: 0,
            vec_base: 0,
            privileged: true,
            irq: false,
            fiq: false,
            run: RunCtl::default(),
        }
    }

    fn check(&self, a: u32, size: u32, write: bool) -> Result<(), MemError> {
        if size == 4 && self.bad.is_some_and(|b| a & !3 == b & !3) {
            return Err(MemError::Abort(Abort {
                va: a,
                status: 5,
                domain: 2,
                write,
            }));
        }
        // u64 で比べる（wasm32 では usize が 32 ビットで、a+size が溢れるため）。
        if a as u64 + size as u64 > self.mem.len() as u64 {
            return Err(MemError::Bus(crate::bus::BusError { addr: a, write }));
        }
        Ok(())
    }

    fn r(&self, a: u32, size: u32) -> u32 {
        let a = a as usize;
        (0..size as usize).fold(0, |v, i| v | (self.mem[a + i] as u32) << (8 * i))
    }

    fn w(&mut self, a: u32, size: u32, v: u32) {
        for i in 0..size as usize {
            self.mem[a as usize + i] = (v >> (8 * i)) as u8;
        }
    }
}

impl System for TestSys {
    fn read(&mut self, va: u32, size: u32) -> Result<u32, MemError> {
        self.check(va, size, false)?;
        Ok(self.r(va, size))
    }
    fn write(&mut self, va: u32, size: u32, v: u32) -> Result<(), MemError> {
        self.check(va, size, true)?;
        self.w(va, size, v);
        Ok(())
    }
    fn fetch32(&mut self, va: u32) -> Result<u32, MemError> {
        self.read(va, 4)
    }
    fn cp15_read(&mut self, _: u8, _: u8, _: u8, _: u8) -> u32 {
        0
    }
    fn cp15_write(&mut self, _: u8, crn: u8, _: u8, _: u8, v: u32) {
        match crn {
            5 => self.fsr = v,
            6 => self.far = v,
            _ => {}
        }
    }
    fn vector_base(&self) -> u32 {
        self.vec_base
    }
    fn set_privileged(&mut self, p: bool) {
        self.privileged = p;
    }
    fn record_data_abort(&mut self, a: &Abort) {
        self.fsr = (a.domain as u32) << 4 | a.status as u32;
        self.far = a.va;
    }
    fn irq(&self) -> bool {
        self.irq
    }
    fn fiq(&self) -> bool {
        self.fiq
    }
    fn run_ctl(&mut self) -> &mut RunCtl {
        &mut self.run
    }
}

const TEST_PC: u32 = 0x1000;

/// PC を TEST_PC にしたコアとシステム。
fn new_core() -> (Cpu, TestSys) {
    let mut s = TestSys::new();
    let mut c = Cpu::new();
    c.reset(TEST_PC, &mut s);
    (c, s)
}

/// word を PC 位置に置いて 1 命令実行する。
fn step(c: &mut Cpu, s: &mut TestSys, word: u32) -> Result<(), StopError> {
    s.w(c.pc(), 4, word);
    c.step(s)
}

#[track_caller]
fn must(c: &mut Cpu, s: &mut TestSys, word: u32) {
    if let Err(e) = step(c, s, word) {
        panic!("step({word:08X}): {e}");
    }
}

/// Thumb 命令を PC 位置に置いて 1 命令実行する。
#[track_caller]
fn must_t(c: &mut Cpu, s: &mut TestSys, hw: u32) {
    s.w(c.pc(), 2, hw);
    if let Err(e) = c.step(s) {
        panic!("step({hw:04X}): {e}");
    }
}

fn new_thumb() -> (Cpu, TestSys) {
    let (mut c, mut s) = new_core();
    let p = c.cpsr | FLAG_T;
    c.set_cpsr(p, &mut s);
    (c, s)
}

// ---- 命令エンコードヘルパー（テストの可読性のため）----

const COND_AL: u32 = 0xE;

/// データ処理・即値形式。imm を rot*2 右ローテートした値がオペランド。
fn dp_imm(op: u32, s: bool, rn: u32, rd: u32, rot: u32, imm: u32) -> u32 {
    COND_AL << 28 | 1 << 25 | op << 21 | (s as u32) << 20 | rn << 16 | rd << 12 | rot << 8 | imm
}

/// データ処理・レジスタ（即値シフト）形式。
fn dp_reg(op: u32, s: bool, rn: u32, rd: u32, rm: u32, shift_type: u32, amount: u32) -> u32 {
    COND_AL << 28
        | op << 21
        | (s as u32) << 20
        | rn << 16
        | rd << 12
        | amount << 7
        | shift_type << 5
        | rm
}

/// データ処理・レジスタ（レジスタ指定シフト）形式。
fn dp_reg_shift(op: u32, s: bool, rn: u32, rd: u32, rm: u32, shift_type: u32, rs: u32) -> u32 {
    COND_AL << 28
        | op << 21
        | (s as u32) << 20
        | rn << 16
        | rd << 12
        | rs << 8
        | shift_type << 5
        | 1 << 4
        | rm
}

/// フラグを "NZCV" の部分文字列で表す（テスト比較用）。
fn flags(p: u32) -> String {
    [(FLAG_N, 'N'), (FLAG_Z, 'Z'), (FLAG_C, 'C'), (FLAG_V, 'V')]
        .iter()
        .filter(|(f, _)| p & f != 0)
        .map(|(_, ch)| *ch)
        .collect()
}

/// CPSR の NZCV を文字列指定で設定する。
fn set_flags(c: &mut Cpu, f: &str) {
    let mut p = c.cpsr & !(FLAG_N | FLAG_Z | FLAG_C | FLAG_V);
    for ch in f.chars() {
        p |= match ch {
            'N' => FLAG_N,
            'Z' => FLAG_Z,
            'C' => FLAG_C,
            'V' => FLAG_V,
            _ => 0,
        };
    }
    c.cpsr = p;
}

fn mode(c: &Cpu) -> u32 {
    c.cpsr & 0x1F
}

fn spsr(c: &Cpu) -> u32 {
    c.spsr[c.cur_bank()]
}

// ---- データ処理: フラグ計算 ----

#[test]
fn data_proc_flags() {
    // (名前, 命令, r1=Rn, r2=Rm/Rs, 入力フラグ, r0 の期待値, 期待フラグ, r0 を書かない)
    let cases: &[(&str, u32, u32, u32, &str, u32, &str, bool)] = &[
        // ADD/ADDS
        (
            "ADDS zero",
            dp_reg(OP_ADD, true, 1, 0, 2, 0, 0),
            0,
            0,
            "",
            0,
            "Z",
            false,
        ),
        (
            "ADDS overflow pos",
            dp_reg(OP_ADD, true, 1, 0, 2, 0, 0),
            0x7FFFFFFF,
            1,
            "",
            0x80000000,
            "NV",
            false,
        ),
        (
            "ADDS carry wrap",
            dp_reg(OP_ADD, true, 1, 0, 2, 0, 0),
            0xFFFFFFFF,
            1,
            "",
            0,
            "ZC",
            false,
        ),
        (
            "ADDS neg+neg overflow",
            dp_reg(OP_ADD, true, 1, 0, 2, 0, 0),
            0x80000000,
            0x80000000,
            "",
            0,
            "ZCV",
            false,
        ),
        (
            "ADDS plain",
            dp_imm(OP_ADD, true, 1, 0, 0, 3),
            4,
            0,
            "",
            7,
            "",
            false,
        ),
        // ADC: キャリー入力
        (
            "ADCS with carry",
            dp_reg(OP_ADC, true, 1, 0, 2, 0, 0),
            0xFFFFFFFF,
            0,
            "C",
            0,
            "ZC",
            false,
        ),
        (
            "ADCS no carry",
            dp_reg(OP_ADC, true, 1, 0, 2, 0, 0),
            0xFFFFFFFF,
            0,
            "",
            0xFFFFFFFF,
            "N",
            false,
        ),
        // SUB: C は「ボローなし」
        (
            "SUBS no borrow",
            dp_imm(OP_SUB, true, 1, 0, 0, 3),
            5,
            0,
            "",
            2,
            "C",
            false,
        ),
        (
            "SUBS borrow",
            dp_imm(OP_SUB, true, 1, 0, 0, 5),
            3,
            0,
            "",
            0xFFFFFFFE,
            "N",
            false,
        ),
        (
            "SUBS equal",
            dp_imm(OP_SUB, true, 1, 0, 0, 7),
            7,
            0,
            "",
            0,
            "ZC",
            false,
        ),
        (
            "SUBS overflow",
            dp_imm(OP_SUB, true, 1, 0, 0, 1),
            0x80000000,
            0,
            "",
            0x7FFFFFFF,
            "CV",
            false,
        ),
        // SBC: C=0 なら追加で 1 引く
        (
            "SBCS carry set",
            dp_imm(OP_SBC, true, 1, 0, 0, 3),
            5,
            0,
            "C",
            2,
            "C",
            false,
        ),
        (
            "SBCS carry clear",
            dp_imm(OP_SBC, true, 1, 0, 0, 3),
            5,
            0,
            "",
            1,
            "C",
            false,
        ),
        // RSB/RSC
        (
            "RSBS",
            dp_imm(OP_RSB, true, 1, 0, 0, 10),
            3,
            0,
            "",
            7,
            "C",
            false,
        ),
        (
            "RSCS carry clear",
            dp_imm(OP_RSC, true, 1, 0, 0, 10),
            3,
            0,
            "",
            6,
            "C",
            false,
        ),
        // 比較・テスト命令（rd は書かれない）
        (
            "CMP equal",
            dp_imm(OP_CMP, true, 1, 0, 0, 9),
            9,
            0,
            "",
            0,
            "ZC",
            true,
        ),
        (
            "CMP less",
            dp_imm(OP_CMP, true, 1, 0, 0, 9),
            5,
            0,
            "",
            0,
            "N",
            true,
        ),
        (
            "CMN",
            dp_imm(OP_CMN, true, 1, 0, 0, 1),
            0xFFFFFFFF,
            0,
            "",
            0,
            "ZC",
            true,
        ),
        (
            "TST zero",
            dp_imm(OP_TST, true, 1, 0, 0, 0xF0),
            0x0F,
            0,
            "",
            0,
            "Z",
            true,
        ),
        (
            "TEQ same",
            dp_reg(OP_TEQ, true, 1, 0, 2, 0, 0),
            0xAA55,
            0xAA55,
            "",
            0,
            "Z",
            true,
        ),
        // 論理系: C はシフタキャリー、V は不変
        (
            "ANDS keeps V",
            dp_imm(OP_AND, true, 1, 0, 0, 0xFF),
            0x80000001,
            0,
            "V",
            1,
            "V",
            false,
        ),
        (
            "ORRS negative",
            dp_imm(OP_ORR, true, 1, 0, 0, 0),
            0x80000000,
            0,
            "",
            0x80000000,
            "N",
            false,
        ),
        (
            "EORS",
            dp_reg(OP_EOR, true, 1, 0, 2, 0, 0),
            0xFF00,
            0x0FF0,
            "",
            0xF0F0,
            "",
            false,
        ),
        (
            "BICS",
            dp_imm(OP_BIC, true, 1, 0, 0, 0x0F),
            0xFF,
            0,
            "",
            0xF0,
            "",
            false,
        ),
        (
            "MOVS zero",
            dp_imm(OP_MOV, true, 0, 0, 0, 0),
            0,
            0,
            "",
            0,
            "Z",
            false,
        ),
        (
            "MVNS",
            dp_imm(OP_MVN, true, 0, 0, 0, 0),
            0,
            0,
            "",
            0xFFFFFFFF,
            "N",
            false,
        ),
        // 即値ローテートのシフタキャリー: rot!=0 なら C = 結果の bit31
        (
            "MOVS imm rot carry",
            dp_imm(OP_MOV, true, 0, 0, 2, 0xFF),
            0,
            0,
            "",
            0xF000000F,
            "NC",
            false,
        ),
        (
            "MOVS imm rot0 keeps C",
            dp_imm(OP_MOV, true, 0, 0, 0, 1),
            0,
            0,
            "C",
            1,
            "C",
            false,
        ),
    ];
    for &(name, word, r1, r2, fin, want, wfl, no_rd) in cases {
        let (mut c, mut s) = new_core();
        c.regs[0] = 0xDEADBEEF;
        c.regs[1] = r1;
        c.regs[2] = r2;
        set_flags(&mut c, fin);
        must(&mut c, &mut s, word);
        let want_rd = if no_rd { 0xDEADBEEF } else { want };
        assert_eq!(c.regs[0], want_rd, "{name}: r0");
        assert_eq!(flags(c.cpsr), wfl, "{name}: flags");
    }
}

// ---- バレルシフタ ----

#[test]
fn shifter_carry() {
    // MOVS r0, r1, <shift> の形で、結果とシフタキャリーを網羅する。
    // (名前, 命令, r1, r2=レジスタ指定シフト量, 入力フラグ, 期待値, 期待フラグ)
    let cases: &[(&str, u32, u32, u32, &str, u32, &str)] = &[
        // 即値シフト
        (
            "LSL #0 keeps C",
            dp_reg(OP_MOV, true, 0, 0, 1, 0, 0),
            2,
            0,
            "C",
            2,
            "C",
        ),
        (
            "LSL #1 carry out",
            dp_reg(OP_MOV, true, 0, 0, 1, 0, 1),
            0x80000001,
            0,
            "",
            2,
            "C",
        ),
        (
            "LSL #4 no carry",
            dp_reg(OP_MOV, true, 0, 0, 1, 0, 4),
            0x0F000001,
            0,
            "",
            0xF0000010,
            "N",
        ),
        (
            "LSR #1",
            dp_reg(OP_MOV, true, 0, 0, 1, 1, 1),
            3,
            0,
            "",
            1,
            "C",
        ),
        (
            "LSR #32 (enc 0)",
            dp_reg(OP_MOV, true, 0, 0, 1, 1, 0),
            0x80000000,
            0,
            "",
            0,
            "ZC",
        ),
        (
            "ASR #1",
            dp_reg(OP_MOV, true, 0, 0, 1, 2, 1),
            0x80000001,
            0,
            "",
            0xC0000000,
            "NC",
        ),
        (
            "ASR #32 (enc 0) neg",
            dp_reg(OP_MOV, true, 0, 0, 1, 2, 0),
            0x80000000,
            0,
            "",
            0xFFFFFFFF,
            "NC",
        ),
        (
            "ASR #32 (enc 0) pos",
            dp_reg(OP_MOV, true, 0, 0, 1, 2, 0),
            0x7FFFFFFF,
            0,
            "",
            0,
            "Z",
        ),
        (
            "ROR #8",
            dp_reg(OP_MOV, true, 0, 0, 1, 3, 8),
            0x000000FF,
            0,
            "",
            0xFF000000,
            "NC",
        ),
        (
            "RRX (ROR #0) C in",
            dp_reg(OP_MOV, true, 0, 0, 1, 3, 0),
            2,
            0,
            "C",
            0x80000001,
            "N",
        ),
        (
            "RRX C out",
            dp_reg(OP_MOV, true, 0, 0, 1, 3, 0),
            1,
            0,
            "",
            0,
            "ZC",
        ),
        // レジスタ指定シフト
        (
            "LSL reg 0 keeps C",
            dp_reg_shift(OP_MOV, true, 0, 0, 1, 0, 2),
            5,
            0,
            "C",
            5,
            "C",
        ),
        (
            "LSL reg 32",
            dp_reg_shift(OP_MOV, true, 0, 0, 1, 0, 2),
            1,
            32,
            "",
            0,
            "ZC",
        ),
        (
            "LSL reg 33",
            dp_reg_shift(OP_MOV, true, 0, 0, 1, 0, 2),
            0xFFFFFFFF,
            33,
            "",
            0,
            "Z",
        ),
        (
            "LSL reg uses low byte",
            dp_reg_shift(OP_MOV, true, 0, 0, 1, 0, 2),
            1,
            0x100,
            "",
            1,
            "",
        ),
        (
            "LSR reg 32",
            dp_reg_shift(OP_MOV, true, 0, 0, 1, 1, 2),
            0x80000000,
            32,
            "",
            0,
            "ZC",
        ),
        (
            "LSR reg 40",
            dp_reg_shift(OP_MOV, true, 0, 0, 1, 1, 2),
            0xFFFFFFFF,
            40,
            "",
            0,
            "Z",
        ),
        (
            "ASR reg 40 neg",
            dp_reg_shift(OP_MOV, true, 0, 0, 1, 2, 2),
            0x80000000,
            40,
            "",
            0xFFFFFFFF,
            "NC",
        ),
        (
            "ROR reg 32 (C=bit31)",
            dp_reg_shift(OP_MOV, true, 0, 0, 1, 3, 2),
            0x80000001,
            32,
            "",
            0x80000001,
            "NC",
        ),
        (
            "ROR reg 4",
            dp_reg_shift(OP_MOV, true, 0, 0, 1, 3, 2),
            0x0000000F,
            4,
            "",
            0xF0000000,
            "NC",
        ),
    ];
    for &(name, word, r1, r2, fin, want, wfl) in cases {
        let (mut c, mut s) = new_core();
        c.regs[1] = r1;
        c.regs[2] = r2;
        set_flags(&mut c, fin);
        must(&mut c, &mut s, word);
        assert_eq!(c.regs[0], want, "{name}: r0");
        assert_eq!(flags(c.cpsr), wfl, "{name}: flags");
    }
}

// ---- 条件実行 ----

#[test]
fn condition_codes() {
    // <cond> MOV r0, #1 を各フラグ状態で実行し、成立/不成立を確認する。
    let cases: &[(u32, &str, bool)] = &[
        (0x0, "Z", true),
        (0x0, "", false), // EQ
        (0x1, "", true),
        (0x1, "Z", false), // NE
        (0x2, "C", true),
        (0x2, "", false), // CS
        (0x3, "", true),
        (0x3, "C", false), // CC
        (0x4, "N", true),
        (0x4, "", false), // MI
        (0x5, "", true),
        (0x5, "N", false), // PL
        (0x6, "V", true),
        (0x6, "", false), // VS
        (0x7, "", true),
        (0x7, "V", false), // VC
        (0x8, "C", true),
        (0x8, "CZ", false),
        (0x8, "", false), // HI
        (0x9, "Z", true),
        (0x9, "", true),
        (0x9, "C", false), // LS
        (0xA, "NV", true),
        (0xA, "", true),
        (0xA, "N", false), // GE
        (0xB, "N", true),
        (0xB, "NV", false), // LT
        (0xC, "NV", true),
        (0xC, "ZNV", false),
        (0xC, "N", false), // GT
        (0xD, "Z", true),
        (0xD, "N", true),
        (0xD, "NV", false), // LE
        (0xE, "", true),    // AL
        // 0xF: Go 版と同じく不成立として飛ばす（cond_passed のコメント参照）
        (0xF, "", false),
        (0xF, "NZCV", false),
    ];
    for &(cond, f, exec) in cases {
        let (mut c, mut s) = new_core();
        set_flags(&mut c, f);
        must(&mut c, &mut s, cond << 28 | 1 << 25 | OP_MOV << 21 | 1); // MOV r0, #1
        assert_eq!(c.regs[0], exec as u32, "cond {cond:X} flags {f:?}");
        assert_eq!(c.pc(), TEST_PC + 4);
    }
    // 表と関数が一致すること
    for i in 0..256u32 {
        assert_eq!(
            COND_TABLE[i as usize],
            cond_passed((i & 15) << 28, i >> 4),
            "{i:02X}"
        );
    }
}

// ---- PC（r15）の見え方 ----

#[test]
fn pc_operand() {
    let (mut c, mut s) = new_core();
    // MOV r0, pc → PC+8
    must(&mut c, &mut s, dp_reg(OP_MOV, false, 0, 0, 15, 0, 0));
    assert_eq!(c.regs[0], TEST_PC + 8);
    // ADD r0, pc, #4 → PC+8+4
    c.reset(TEST_PC, &mut s);
    must(&mut c, &mut s, dp_imm(OP_ADD, false, 15, 0, 0, 4));
    assert_eq!(c.regs[0], TEST_PC + 12);
    // MOV pc, r1 → 分岐（bit1:0 は無視される）
    c.reset(TEST_PC, &mut s);
    c.regs[1] = 0x2003;
    must(&mut c, &mut s, dp_reg(OP_MOV, false, 0, 15, 1, 0, 0));
    assert_eq!(c.pc(), 0x2000);
}

// ---- 分岐 ----

#[test]
fn branch() {
    let (mut c, mut s) = new_core();
    // B +8 (offset フィールド = 2): 飛び先 = PC+8+8
    must(&mut c, &mut s, 0xEA000000 | 2);
    assert_eq!(c.pc(), TEST_PC + 16);
    // B 後方: offset = -4 命令 (0xFFFFFC)
    c.reset(TEST_PC, &mut s);
    must(&mut c, &mut s, 0xEA000000 | 0x00FFFFFC);
    assert_eq!(c.pc(), TEST_PC + 8 - 16);
    // BL: lr = 次の命令
    c.reset(TEST_PC, &mut s);
    must(&mut c, &mut s, 0xEB000000 | 2);
    assert_eq!(c.regs[14], TEST_PC + 4);
    assert_eq!(c.pc(), TEST_PC + 16);
}

#[test]
fn bx() {
    // ARM のまま分岐
    let (mut c, mut s) = new_core();
    c.regs[3] = 0x3000;
    must(&mut c, &mut s, 0xE12FFF10 | 3);
    assert!(c.pc() == 0x3000 && !c.thumb());
    // Thumb へ切り替え → 次の step は Thumb 命令として実行される
    c.reset(TEST_PC, &mut s);
    c.regs[3] = 0x3001;
    must(&mut c, &mut s, 0xE12FFF10 | 3);
    assert!(c.pc() == 0x3000 && c.thumb());
    s.w(0x3000, 2, 0x2107); // mov r1, #7
    c.step(&mut s).unwrap();
    assert_eq!((c.reg(1), c.pc()), (7, 0x3002));
}

// ---- ロード/ストア ----

#[test]
fn load_store_word() {
    let (mut c, mut s) = new_core();
    c.regs[1] = 0x2000;
    c.regs[2] = 0x11223344;
    must(&mut c, &mut s, 0xE5812004); // STR r2, [r1, #4]
    assert_eq!(s.r(0x2004, 4), 0x11223344);
    must(&mut c, &mut s, 0xE5910004); // LDR r0, [r1, #4]
    assert_eq!(c.regs[0], 0x11223344);
    // プリインデックス+ライトバック: LDR r0, [r1, #4]!
    c.regs[1] = 0x2000;
    must(&mut c, &mut s, 0xE5B10004);
    assert_eq!((c.regs[0], c.regs[1]), (0x11223344, 0x2004));
    // ポストインデックス: LDR r0, [r1], #4（アクセスは旧ベース、r1 は +4）
    c.regs[1] = 0x2004;
    c.regs[0] = 0;
    must(&mut c, &mut s, 0xE4910004);
    assert_eq!((c.regs[0], c.regs[1]), (0x11223344, 0x2008));
    // 減算オフセット: LDR r0, [r1, #-4]（r1=0x2008）
    c.regs[0] = 0;
    must(&mut c, &mut s, 0xE5110004);
    assert_eq!(c.regs[0], 0x11223344);
    // レジスタオフセット（スケーリング付き）: LDR r0, [r1, r3, LSL #2]
    c.regs[1] = 0x2000;
    c.regs[3] = 1;
    c.regs[0] = 0;
    must(&mut c, &mut s, 0xE7910103);
    assert_eq!(c.regs[0], 0x11223344);
}

#[test]
fn load_store_byte_half() {
    let (mut c, mut s) = new_core();
    c.regs[1] = 0x2000;
    c.regs[2] = 0x11223344;
    must(&mut c, &mut s, 0xE5C12000); // STRB r2, [r1]
    assert_eq!(s.r(0x2000, 1), 0x44);
    must(&mut c, &mut s, 0xE5D10000); // LDRB r0, [r1]
    assert_eq!(c.regs[0], 0x44);
    must(&mut c, &mut s, 0xE1C120B2); // STRH r2, [r1, #2]
    assert_eq!(s.r(0x2002, 2), 0x3344);
    must(&mut c, &mut s, 0xE1D100B2); // LDRH r0, [r1, #2]
    assert_eq!(c.regs[0], 0x3344);
    must(&mut c, &mut s, 0xE1D100D0); // LDRSB r0, [r1]（0x44 → 正: そのまま）
    assert_eq!(c.regs[0], 0x44);
    s.w(0x2000, 1, 0x80);
    must(&mut c, &mut s, 0xE1D100D0); // 負値
    assert_eq!(c.regs[0], 0xFFFFFF80);
    must(&mut c, &mut s, 0xE1D100F2); // LDRSH r0, [r1, #2]（0x3344 → 正）
    assert_eq!(c.regs[0], 0x3344);
}

#[test]
fn load_unaligned_rotate() {
    // ARMv4 の非アラインワードロードはロートされた値になる。
    let (mut c, mut s) = new_core();
    s.w(0x2000, 4, 0x11223344);
    c.regs[1] = 0x2001;
    must(&mut c, &mut s, 0xE5910000); // LDR r0, [r1]
    assert_eq!(c.regs[0], 0x44112233);
}

#[test]
fn ldr_same_reg_wins() {
    // LDR r1, [r1], #4: ロード値がライトバックに勝つ
    let (mut c, mut s) = new_core();
    s.w(0x2000, 4, 0xCAFEBABE);
    c.regs[1] = 0x2000;
    must(&mut c, &mut s, 0xE4911004);
    assert_eq!(c.regs[1], 0xCAFEBABE);
}

// ---- LDM/STM ----

#[test]
fn ldm_stm() {
    let (mut c, mut s) = new_core();
    (c.regs[0], c.regs[1], c.regs[2]) = (0xAAAA0000, 0xBBBB1111, 0xCCCC2222);
    // STMDB r13!, {r0-r2}（プッシュ）
    c.regs[13] = 0x3000;
    must(&mut c, &mut s, 0xE92D0007);
    assert_eq!(c.regs[13], 0x3000 - 12);
    for (i, want) in [0xAAAA0000, 0xBBBB1111, 0xCCCC2222].into_iter().enumerate() {
        assert_eq!(s.r(0x2FF4 + 4 * i as u32, 4), want);
    }
    // LDMIA r13!, {r3-r5}（ポップ）
    must(&mut c, &mut s, 0xE8BD0038);
    assert_eq!(c.regs[3..6], [0xAAAA0000, 0xBBBB1111, 0xCCCC2222]);
    assert_eq!(c.regs[13], 0x3000);
    // STMIB r6, {r0, r1}（ライトバックなし）: 格納先は base+4, base+8
    c.regs[6] = 0x3100;
    must(&mut c, &mut s, 0xE9860003);
    assert_eq!((s.r(0x3104, 4), s.r(0x3108, 4)), (0xAAAA0000, 0xBBBB1111));
    assert_eq!(c.regs[6], 0x3100);
    // LDMDA r6!, {r7, r8}: 「小さいレジスタ=小さいアドレス」
    c.regs[6] = 0x3108;
    must(&mut c, &mut s, 0xE8360180);
    assert_eq!((c.regs[7], c.regs[8]), (0xAAAA0000, 0xBBBB1111));
    assert_eq!(c.regs[6], 0x3100);
}

#[test]
fn ldm_to_pc() {
    let (mut c, mut s) = new_core();
    s.w(0x3000, 4, 0x12345678);
    s.w(0x3004, 4, 0x00004000); // → PC
    c.regs[13] = 0x3000;
    must(&mut c, &mut s, 0xE8BD8001); // LDMIA r13!, {r0, pc}
    assert_eq!(
        (c.regs[0], c.pc(), c.regs[13]),
        (0x12345678, 0x4000, 0x3008)
    );
}

// ---- MRS/MSR とモード切替・バンク ----

#[test]
fn msr_mode_switch_banking() {
    let (mut c, mut s) = new_core(); // reset 直後は SVC モード
    assert_eq!(mode(&c), MODE_SVC);
    c.regs[13] = 0x1111; // SVC の sp
    c.regs[14] = 0x2222; // SVC の lr
    // MSR CPSR_c, r4 → IRQ モードへ
    c.regs[4] = MODE_IRQ | 0xC0;
    must(&mut c, &mut s, 0xE121F004);
    assert_eq!(mode(&c), MODE_IRQ);
    // IRQ の r13/r14 は独立（初期値 0）
    assert_eq!((c.regs[13], c.regs[14]), (0, 0));
    c.regs[13] = 0x3333;
    // SVC へ戻ると r13/r14 が復元される
    c.regs[4] = MODE_SVC | 0xC0;
    must(&mut c, &mut s, 0xE121F004);
    assert_eq!((c.regs[13], c.regs[14]), (0x1111, 0x2222));
    // r0-r7 はバンクされない
    c.regs[0] = 0x7777;
    c.regs[4] = MODE_IRQ | 0xC0;
    must(&mut c, &mut s, 0xE121F004);
    assert_eq!(c.regs[0], 0x7777);
    assert_eq!(c.regs[13], 0x3333);
}

#[test]
fn fiq_banking() {
    let (mut c, mut s) = new_core();
    c.regs[8] = 0x88;
    c.regs[12] = 0xCC;
    // FIQ モードへ: r8-r12 も切り替わる
    c.regs[4] = MODE_FIQ | 0xC0;
    must(&mut c, &mut s, 0xE121F004);
    assert_eq!((c.regs[8], c.regs[12]), (0, 0));
    c.regs[8] = 0xF8;
    c.regs[4] = MODE_SVC | 0xC0;
    must(&mut c, &mut s, 0xE121F004);
    assert_eq!((c.regs[8], c.regs[12]), (0x88, 0xCC));
}

#[test]
fn mrs() {
    let (mut c, mut s) = new_core();
    set_flags(&mut c, "NC");
    must(&mut c, &mut s, 0xE10F0000); // MRS r0, CPSR
    assert_eq!(c.regs[0], FLAG_N | FLAG_C | FLAG_I | FLAG_F | MODE_SVC);
}

#[test]
fn msr_user_mode_restriction() {
    let (mut c, mut s) = new_core();
    // まず usr モードに落とす（特権の SVC から）
    c.regs[4] = MODE_USR | 0xC0;
    must(&mut c, &mut s, 0xE121F004);
    assert_eq!(mode(&c), MODE_USR);
    assert!(!s.privileged, "usr への切替で MMU に非特権を伝える");
    // usr モードから MSR CPSR_c でモードを変えようとしても無視される
    c.regs[4] = MODE_SVC;
    must(&mut c, &mut s, 0xE121F004);
    assert_eq!(mode(&c), MODE_USR);
    // フラグは書ける: MSR CPSR_f, r4
    c.regs[4] = FLAG_N | FLAG_Z;
    must(&mut c, &mut s, 0xE128F004);
    assert_eq!(flags(c.cpsr), "NZ");
}

// ---- SWI（例外エントリ）----

#[test]
fn swi() {
    let (mut c, mut s) = new_core();
    // いったん IRQ モードにして、SWI で SVC に入ることを確認
    c.regs[4] = MODE_IRQ | 0xC0;
    must(&mut c, &mut s, 0xE121F004);
    let old = c.cpsr;
    must(&mut c, &mut s, 0xEF000042); // SWI #0x42
    assert_eq!(mode(&c), MODE_SVC);
    assert_eq!(c.pc(), VEC_SWI);
    assert_eq!(c.regs[14], TEST_PC + 4 + 4);
    assert_eq!(spsr(&c), old);
    assert_ne!(c.cpsr & FLAG_I, 0);
}

// ---- 例外復帰イディオム ----

#[test]
fn exception_return() {
    let (mut c, mut s) = new_core();
    // SWI で SVC に入り、SPSR に IRQ モードを持たせてから MOVS pc, lr
    c.regs[4] = MODE_IRQ | 0xC0;
    must(&mut c, &mut s, 0xE121F004);
    must(&mut c, &mut s, 0xEF000000); // SWI → SVC, lr=TEST_PC+8, SPSR=IRQ
    must(&mut c, &mut s, 0xE1B0F00E); // MOVS pc, lr
    assert_eq!(mode(&c), MODE_IRQ);
    assert_eq!(c.pc(), TEST_PC + 8);
}

// ---- 未実装命令の報告 ----

#[test]
fn undefined_reports_pc_and_word() {
    let (mut c, mut s) = new_core();
    const BAD_LDM: u32 = 0xE8B10000; // 空レジスタリストの LDM（UNPREDICTABLE → 停止）
    match step(&mut c, &mut s, BAD_LDM) {
        Err(StopError::Undefined(u)) => {
            assert_eq!((u.pc, u.word, u.arch), (TEST_PC, BAD_LDM, false));
        }
        other => panic!("got {other:?}, want Undefined"),
    }
    // PC は命令位置に戻っている（停止位置の報告用）
    assert_eq!(c.pc(), TEST_PC);
    // エラーを起こした命令も 1 命令と数える
    assert_eq!(s.run.n, 1);
}

#[test]
fn bus_error_stops() {
    let (mut c, mut s) = new_core();
    c.regs[1] = 0x20000; // 64KB の外
    assert_eq!(
        step(&mut c, &mut s, 0xE5910000), // LDR r0, [r1]
        Err(StopError::Bus(crate::bus::BusError {
            addr: 0x20000,
            write: false
        }))
    );
    assert_eq!(c.pc(), TEST_PC);
}

// ---- 例外配送（Go の exception_test）----

fn new_abort_core(bad: u32) -> (Cpu, TestSys) {
    let (c, mut s) = new_core();
    s.bad = Some(bad);
    (c, s)
}

#[test]
fn data_abort_delivery() {
    let (mut c, mut s) = new_abort_core(0x8000);
    c.regs[1] = 0x8000;
    must(&mut c, &mut s, 0xE5910000); // LDR r0, [r1] → データアボート
    assert_eq!(mode(&c), MODE_ABT);
    assert_eq!(c.pc(), VEC_DABT);
    assert_eq!(c.regs[14], TEST_PC + 8, "LR = PC+8");
    assert_eq!(s.far, 0x8000);
    assert_eq!(s.fsr, 2 << 4 | 0x5, "domain<<4|status");
    // SPSR に元の CPSR（SVC）が入っている
    assert_eq!(spsr(&c) & 0x1F, MODE_SVC);
    // I ビットが立ち、アボートは ARM state で受ける
    assert!(c.cpsr & FLAG_I != 0 && !c.thumb());
}

#[test]
fn prefetch_abort_delivery() {
    let (mut c, mut s) = new_abort_core(0x8000);
    c.regs[15] = 0x8000; // 実行不能アドレスへ飛んだ状態
    c.step(&mut s).unwrap();
    assert_eq!(mode(&c), MODE_ABT);
    assert_eq!(c.pc(), VEC_PABT);
    assert_eq!(c.regs[14], 0x8000 + 4, "LR = PC+4");
    // プリフェッチアボートでは FSR/FAR は更新されない
    assert_eq!((s.fsr, s.far), (0, 0));
}

#[test]
fn high_vectors() {
    let (mut c, mut s) = new_abort_core(0x8000);
    s.vec_base = 0xFFFF0000;
    c.regs[1] = 0x8000;
    must(&mut c, &mut s, 0xE5910000);
    assert_eq!(c.pc(), 0xFFFF0000 | VEC_DABT);
}

#[test]
fn irq_delivery() {
    let (mut c, mut s) = new_core();
    // I ビットを落とす（reset 直後は禁止されている）
    let p = c.cpsr & !FLAG_I;
    c.set_cpsr(p, &mut s);
    s.irq = true;
    c.step(&mut s).unwrap();
    assert_eq!(mode(&c), MODE_IRQ);
    assert_eq!(c.pc(), VEC_IRQ);
    assert_eq!(c.regs[14], TEST_PC + 4, "未実行命令+4");
    assert_ne!(c.cpsr & FLAG_I, 0);
    assert_ne!(c.cpsr & FLAG_F, 0, "F は変化しない");
    assert_eq!(s.run.n, 1, "割り込みの受け付けも 1 命令と数える");
}

#[test]
fn irq_masked_by_i_flag() {
    let (mut c, mut s) = new_core();
    s.irq = true; // I=1 のままなので配送されない
    must(&mut c, &mut s, 0xE1A00000); // MOV r0, r0 (NOP)
    assert_eq!(mode(&c), MODE_SVC);
    assert_eq!(c.pc(), TEST_PC + 4);
}

#[test]
fn fiq_beats_irq() {
    let (mut c, mut s) = new_core();
    let p = c.cpsr & !(FLAG_I | FLAG_F);
    c.set_cpsr(p, &mut s);
    s.irq = true;
    s.fiq = true;
    c.step(&mut s).unwrap();
    assert_eq!(mode(&c), MODE_FIQ);
    assert_eq!(c.pc(), VEC_FIQ);
    // FIQ エントリでは I/F 両方が禁止される
    assert!(c.cpsr & FLAG_I != 0 && c.cpsr & FLAG_F != 0);
}

#[test]
fn ldm_abort_restores_base() {
    // base restored モデル: ライトバック済みでもアボート時はベースを戻す。
    let (mut c, mut s) = new_abort_core(0x8000);
    c.regs[1] = 0x7FF8; // r1 を起点に 4 ワードロード → 3 ワード目 (0x8000) でアボート
    must(&mut c, &mut s, 0xE8B1003C); // LDMIA r1!, {r2-r5}
    assert_eq!(mode(&c), MODE_ABT);
    assert_eq!(c.reg(1), 0x7FF8, "base restored");
}

#[test]
fn ldm_stm_user_bank() {
    let (mut c, mut s) = new_core();
    // SVC モードで usr の r13/r14 に値を仕込む: 一旦 SYS に切り替えて書き、SVC に戻す
    c.set_cpsr(MODE_SYS | FLAG_I | FLAG_F, &mut s);
    c.set_reg(13, 0x1111);
    c.set_reg(14, 0x2222);
    c.set_reg(8, 0x3333);
    c.set_cpsr(MODE_SVC | FLAG_I | FLAG_F, &mut s);
    c.set_reg(13, 0xAAAA); // SVC 側の r13/r14 は別値
    c.set_reg(14, 0xBBBB);
    c.set_reg(0, 0x4000);
    // STMIA r0, {r8,r13,r14}^ → usr バンクの値が格納される
    must(&mut c, &mut s, 0xE8C06100);
    for (i, want) in [0x3333, 0x1111, 0x2222].into_iter().enumerate() {
        assert_eq!(s.r(0x4000 + 4 * i as u32, 4), want, "STM(2) word {i}");
    }
    // メモリを書き換えて LDMIA r0, {r8,r13,r14}^ → usr バンクに入り、SVC 側は不変
    for (i, v) in [0x5555, 0x6666, 0x7777].into_iter().enumerate() {
        s.w(0x4000 + 4 * i as u32, 4, v);
    }
    must(&mut c, &mut s, 0xE8D06100);
    assert_eq!((c.reg(13), c.reg(14)), (0xAAAA, 0xBBBB));
    c.set_cpsr(MODE_SYS | FLAG_I | FLAG_F, &mut s);
    assert_eq!((c.reg(8), c.reg(13), c.reg(14)), (0x5555, 0x6666, 0x7777));
}

#[test]
fn stm_user_bank_from_fiq() {
    // FIQ モードでは r8-r12 も usr バンク側が格納される
    let (mut c, mut s) = new_core();
    c.set_reg(8, 0x1234); // SVC(=usr と共有の r8) に値
    c.set_cpsr(MODE_FIQ | FLAG_I | FLAG_F, &mut s);
    c.set_reg(8, 0xFFFF); // FIQ バンクの r8
    c.set_reg(0, 0x4000);
    must(&mut c, &mut s, 0xE8C00100); // STMIA r0, {r8}^
    assert_eq!(s.r(0x4000, 4), 0x1234);
}

#[test]
fn arch_undefined_delivery() {
    // 存在しないコプロセッサへの MRC は実機同様に未定義命令例外になる
    // （WinCE の FPU 検出が依存する）。
    let (mut c, mut s) = new_core();
    must(&mut c, &mut s, 0xEEF80A10); // MRC p10（VFP）
    assert_eq!(mode(&c), MODE_UND);
    assert_eq!(c.pc(), VEC_UNDEF);
    assert_eq!(c.regs[14], TEST_PC + 4);
}

// ---- 乗算（Go の mul_test）----

/// MUL/MLA。a=true で MLA（+Rn）。
fn mul_enc(s: bool, a: bool, rd: u32, rn: u32, rs: u32, rm: u32) -> u32 {
    COND_AL << 28
        | (a as u32) << 21
        | (s as u32) << 20
        | rd << 16
        | rn << 12
        | rs << 8
        | 9 << 4
        | rm
}

/// UMULL/UMLAL/SMULL/SMLAL。
fn mul_long_enc(s: bool, signed: bool, a: bool, rd_hi: u32, rd_lo: u32, rs: u32, rm: u32) -> u32 {
    COND_AL << 28
        | 1 << 23
        | (signed as u32) << 22
        | (a as u32) << 21
        | (s as u32) << 20
        | rd_hi << 16
        | rd_lo << 12
        | rs << 8
        | 9 << 4
        | rm
}

/// SWP/SWPB。
fn swp_enc(byte: bool, rn: u32, rd: u32, rm: u32) -> u32 {
    COND_AL << 28 | 1 << 24 | (byte as u32) << 22 | rn << 16 | rd << 12 | 9 << 4 | rm
}

#[test]
fn mul_mla() {
    // (名前, S, A, Rm, Rs, Rn, 期待値, 入力フラグ, 期待フラグ)
    let cases: &[(&str, bool, bool, u32, u32, u32, u32, &str, &str)] = &[
        ("MUL 基本", false, false, 6, 7, 0, 42, "", ""),
        (
            "MUL 符号は関係ない(mod 2^32)",
            false,
            false,
            0xFFFFFFFF,
            3,
            0,
            0xFFFFFFFD,
            "",
            "",
        ),
        ("MLA 加算", false, true, 5, 4, 100, 120, "", ""),
        ("MULS N", true, false, 0x80000000, 1, 0, 0x80000000, "", "N"),
        ("MULS Z", true, false, 0, 123, 0, 0, "", "Z"),
        // ARMv4 の MULS で C は UNPREDICTABLE。本実装は「不変」なので保存される。
        ("MULS C/V 不変", true, false, 2, 3, 0, 6, "CV", "CV"),
        ("MLAS でも同様", true, true, 2, 3, 4, 10, "C", "C"),
    ];
    for &(name, sb, a, rm, rs, rn, want, fin, wfl) in cases {
        let (mut c, mut s) = new_core();
        set_flags(&mut c, fin);
        c.set_reg(1, rm);
        c.set_reg(2, rs);
        c.set_reg(3, rn);
        must(&mut c, &mut s, mul_enc(sb, a, 4, 3, 2, 1));
        assert_eq!(c.reg(4), want, "{name}");
        assert_eq!(flags(c.cpsr), wfl, "{name}");
    }
}

#[test]
fn mul_long() {
    // (名前, 符号付き, A, S, Rm, Rs, hi 入力, lo 入力, hi 期待, lo 期待, 期待フラグ)
    let cases: &[(&str, bool, bool, bool, u32, u32, u32, u32, u32, u32, &str)] = &[
        (
            "UMULL 基本",
            false,
            false,
            false,
            0xFFFFFFFF,
            0xFFFFFFFF,
            0,
            0,
            0xFFFFFFFE,
            0x00000001,
            "",
        ),
        (
            "UMULL 小さい値",
            false,
            false,
            false,
            1000,
            1000,
            0,
            0,
            0,
            1000000,
            "",
        ),
        (
            "SMULL 負×正",
            true,
            false,
            false,
            0xFFFFFFFF,
            5,
            0,
            0,
            0xFFFFFFFF,
            0xFFFFFFFB,
            "",
        ),
        (
            "SMULL 負×負",
            true,
            false,
            false,
            0xFFFFFFFE,
            0xFFFFFFFD,
            0,
            0,
            0,
            6,
            "",
        ),
        (
            "UMLAL 加算",
            false,
            true,
            false,
            2,
            3,
            1,
            0xFFFFFFFF,
            2,
            5,
            "",
        ),
        (
            "SMLAL 加算",
            true,
            true,
            false,
            0xFFFFFFFF,
            1,
            0,
            5,
            0,
            4,
            "",
        ),
        ("UMULLS Z", false, false, true, 0, 12345, 0, 0, 0, 0, "Z"),
        (
            "SMULLS N (bit63)",
            true,
            false,
            true,
            0xFFFFFFFF,
            1,
            0,
            0,
            0xFFFFFFFF,
            0xFFFFFFFF,
            "N",
        ),
    ];
    for &(name, signed, a, sb, rm, rs, hi_in, lo_in, hi, lo, wfl) in cases {
        let (mut c, mut s) = new_core();
        c.set_reg(1, rm);
        c.set_reg(2, rs);
        c.set_reg(4, hi_in);
        c.set_reg(3, lo_in);
        must(&mut c, &mut s, mul_long_enc(sb, signed, a, 4, 3, 2, 1));
        assert_eq!((c.reg(4), c.reg(3)), (hi, lo), "{name}");
        assert_eq!(flags(c.cpsr), wfl, "{name}");
    }
}

#[test]
fn swp() {
    let (mut c, mut s) = new_core();
    s.w(0x2000, 4, 0x11223344);
    c.set_reg(1, 0x2000); // Rn: アドレス
    c.set_reg(2, 0xAABBCCDD); // Rm: 書き込む値
    must(&mut c, &mut s, swp_enc(false, 1, 3, 2));
    assert_eq!(c.reg(3), 0x11223344);
    assert_eq!(s.r(0x2000, 4), 0xAABBCCDD);
}

#[test]
fn swp_unaligned_rotate() {
    // 非アラインアドレスのロード値は LDR と同じ回転。ストアはアラインされる。
    let (mut c, mut s) = new_core();
    s.w(0x2000, 4, 0x11223344);
    c.set_reg(1, 0x2001);
    c.set_reg(2, 0xAABBCCDD);
    must(&mut c, &mut s, swp_enc(false, 1, 3, 2));
    assert_eq!(c.reg(3), ror(0x11223344, 8));
    assert_eq!(s.r(0x2000, 4), 0xAABBCCDD);
}

#[test]
fn swpb() {
    let (mut c, mut s) = new_core();
    s.w(0x2000, 4, 0x11223344);
    c.set_reg(1, 0x2002);
    c.set_reg(2, 0xFF);
    must(&mut c, &mut s, swp_enc(true, 1, 3, 2));
    assert_eq!(c.reg(3), 0x22);
    assert_eq!(s.r(0x2000, 4), 0x11FF3344);
}

// ---- Thumb（Go の thumb_test）----

#[test]
fn thumb_shift_imm() {
    // LSL Rd, Rs, #imm: 000 00 imm5 rs rd
    let cases: &[(&str, u32, u32, u32, &str)] = &[
        ("LSL #4", 0x0111, 0x0F0F, 0xF0F0, ""),
        ("LSL #0 (MOV)", 0x0011, 0x80000001, 0x80000001, "N"),
        ("LSR #1 C", 0x0851, 0x3, 0x1, "C"),
        ("LSR #0 = #32", 0x0811, 0x80000000, 0, "ZC"),
        ("ASR #1", 0x1051, 0x80000002, 0xC0000001, "N"),
    ];
    for &(name, hw, rs, want, wfl) in cases {
        let (mut c, mut s) = new_thumb();
        c.set_reg(2, rs);
        must_t(&mut c, &mut s, hw);
        assert_eq!(c.reg(1), want, "{name}");
        assert_eq!(flags(c.cpsr), wfl, "{name}");
    }
}

#[test]
fn thumb_add_sub() {
    let (mut c, mut s) = new_thumb();
    c.set_reg(2, 100);
    c.set_reg(3, 42);
    must_t(&mut c, &mut s, 0x18D1); // add r1, r2, r3
    assert_eq!(c.reg(1), 142);
    must_t(&mut c, &mut s, 0x1AD1); // sub r1, r2, r3
    assert!(c.reg(1) == 58 && c.flag_c());
    must_t(&mut c, &mut s, 0x1DD1); // add r1, r2, #7
    assert_eq!(c.reg(1), 107);
    must_t(&mut c, &mut s, 0x1FD1); // sub r1, r2, #7
    assert_eq!(c.reg(1), 93);
}

#[test]
fn thumb_imm8() {
    let (mut c, mut s) = new_thumb();
    must_t(&mut c, &mut s, 0x21FF); // mov r1, #0xFF
    assert_eq!(c.reg(1), 0xFF);
    must_t(&mut c, &mut s, 0x3105); // add r1, #5
    assert_eq!(c.reg(1), 0x104);
    must_t(&mut c, &mut s, 0x3904); // sub r1, #4
    assert_eq!(c.reg(1), 0x100);
}

#[test]
fn thumb_cmp_imm8() {
    let (mut c, mut s) = new_thumb();
    c.set_reg(1, 5);
    must_t(&mut c, &mut s, 0x2905); // cmp r1, #5
    assert_eq!(flags(c.cpsr), "ZC");
    must_t(&mut c, &mut s, 0x2906); // cmp r1, #6
    assert_eq!(flags(c.cpsr), "N");
}

#[test]
fn thumb_alu() {
    // (名前, op=bits[9:6], rd 入力, rs 入力, 期待値, 入力フラグ, 期待フラグ, rd 不変)
    let cases: &[(&str, u32, u32, u32, u32, &str, &str, bool)] = &[
        ("AND", 0x0, 0xFF00FF, 0x00FFFF, 0x0000FF, "", "", false),
        ("EOR", 0x1, 0xFF, 0x0F, 0xF0, "", "", false),
        ("LSL reg", 0x2, 1, 8, 0x100, "", "", false),
        ("LSR reg", 0x3, 0x100, 8, 1, "", "", false),
        ("ASR reg", 0x4, 0x80000000, 31, 0xFFFFFFFF, "", "N", false), // 最後に出るのは bit30=0 → C=0
        ("ADC C=1", 0x5, 10, 20, 31, "C", "", false),                 // キャリーアウトなし → C=0
        ("SBC C=0", 0x6, 10, 5, 4, "", "C", false),
        ("ROR", 0x7, 0xF000000F, 4, 0xFF000000, "", "NC", false),
        ("TST", 0x8, 0xF0, 0x0F, 0xF0, "", "Z", true),
        ("NEG", 0x9, 0, 5, 0xFFFFFFFB, "", "N", false),
        ("CMP eq", 0xA, 7, 7, 7, "", "ZC", true),
        ("CMN", 0xB, 1, 0xFFFFFFFF, 1, "", "ZC", true),
        ("ORR", 0xC, 0xF0, 0x0F, 0xFF, "", "", false),
        ("MUL", 0xD, 6, 7, 42, "", "", false),
        ("MUL C保存", 0xD, 6, 7, 42, "C", "C", false),
        ("BIC", 0xE, 0xFF, 0x0F, 0xF0, "", "", false),
        ("MVN", 0xF, 0, 0xFFFFFF00, 0xFF, "", "", false),
    ];
    for &(name, op, rd, rs, want, fin, wfl, test_only) in cases {
        let (mut c, mut s) = new_thumb();
        set_flags(&mut c, fin);
        c.set_reg(1, rd);
        c.set_reg(2, rs);
        must_t(&mut c, &mut s, 0x4000 | op << 6 | 2 << 3 | 1);
        assert_eq!(c.reg(1), if test_only { rd } else { want }, "{name}");
        assert_eq!(flags(c.cpsr), wfl, "{name}");
    }
}

#[test]
fn thumb_hi_reg_ops() {
    let (mut c, mut s) = new_thumb();
    c.set_reg(1, 10);
    c.set_reg(9, 32);
    must_t(&mut c, &mut s, 0x4449); // add r1, r9
    assert_eq!(c.reg(1), 42);
    must_t(&mut c, &mut s, 0x46C9); // mov r9, r9
    must_t(&mut c, &mut s, 0x468A); // mov r10, r1
    assert_eq!(c.reg(10), 42);
    set_flags(&mut c, "");
    must_t(&mut c, &mut s, 0x4551); // cmp r1, r10
    assert_eq!(flags(c.cpsr), "ZC");
}

#[test]
fn thumb_bx() {
    // Thumb → ARM
    let (mut c, mut s) = new_thumb();
    c.set_reg(3, 0x2000); // bit0=0 → ARM へ
    must_t(&mut c, &mut s, 0x4718); // bx r3
    assert!(!c.thumb());
    assert_eq!(c.pc(), 0x2000);
    // ARM 側で BX で Thumb に戻る
    c.set_reg(4, 0x3001);
    must(&mut c, &mut s, 0xE12FFF14); // bx r4
    assert!(c.thumb() && c.pc() == 0x3000);
}

#[test]
fn thumb_pc_relative_load() {
    let (mut c, mut s) = new_thumb();
    // PC=0x1000。ベース = (0x1000+4)&!3 = 0x1004。imm=4 → 0x1014
    s.w(0x1014, 4, 0xCAFEF00D);
    must_t(&mut c, &mut s, 0x4904); // ldr r1, [pc, #16]
    assert_eq!(c.reg(1), 0xCAFEF00D);
}

#[test]
fn thumb_load_store() {
    let (mut c, mut s) = new_thumb();
    c.set_reg(1, 0x2000);
    c.set_reg(2, 4);
    c.set_reg(3, 0xAABBCCDD);
    must_t(&mut c, &mut s, 0x508B); // str r3, [r1, r2]
    assert_eq!(s.r(0x2004, 4), 0xAABBCCDD);
    must_t(&mut c, &mut s, 0x588C); // ldr r4, [r1, r2]
    assert_eq!(c.reg(4), 0xAABBCCDD);
    must_t(&mut c, &mut s, 0x684D); // ldr r5, [r1, #4]
    assert_eq!(c.reg(5), 0xAABBCCDD);
    must_t(&mut c, &mut s, 0x708B); // strb r3, [r1, #2]
    assert_eq!(s.r(0x2002, 1), 0xDD);
    must_t(&mut c, &mut s, 0x788C); // ldrb r4, [r1, #2]
    assert_eq!(c.reg(4), 0xDD);
    must_t(&mut c, &mut s, 0x800B); // strh r3, [r1, #0]
    assert_eq!(s.r(0x2000, 2), 0xCCDD);
    must_t(&mut c, &mut s, 0x880C); // ldrh r4, [r1, #0]
    assert_eq!(c.reg(4), 0xCCDD);
    // 符号付きロード（[r1+r2] = 0x2004 には 0xAABBCCDD が入っている）
    must_t(&mut c, &mut s, 0x568C); // ldrsb r4, [r1, r2]
    assert_eq!(c.reg(4), 0xFFFFFFDD);
    must_t(&mut c, &mut s, 0x5E8C); // ldrsh r4, [r1, r2]
    assert_eq!(c.reg(4), 0xFFFFCCDD);
}

#[test]
fn thumb_sp_relative() {
    let (mut c, mut s) = new_thumb();
    c.set_reg(13, 0x4000);
    c.set_reg(1, 0x12345678);
    must_t(&mut c, &mut s, 0x9102); // str r1, [sp, #8]
    assert_eq!(s.r(0x4008, 4), 0x12345678);
    must_t(&mut c, &mut s, 0x9A02); // ldr r2, [sp, #8]
    assert_eq!(c.reg(2), 0x12345678);
    must_t(&mut c, &mut s, 0xA903); // add r1, sp, #12
    assert_eq!(c.reg(1), 0x400C);
    must_t(&mut c, &mut s, 0xA201); // add r2, pc, #4 → (PC+4)&!3 + 4
    assert_eq!(c.reg(2), ((c.pc() - 2 + 4) & !3) + 4);
    must_t(&mut c, &mut s, 0xB082); // sub sp, #8
    assert_eq!(c.reg(13), 0x3FF8);
    must_t(&mut c, &mut s, 0xB002); // add sp, #8
    assert_eq!(c.reg(13), 0x4000);
}

#[test]
fn thumb_push_pop() {
    let (mut c, mut s) = new_thumb();
    c.set_reg(13, 0x4000);
    c.set_reg(1, 0x11);
    c.set_reg(2, 0x22);
    c.set_reg(14, 0x1235); // LR（Thumb の戻り先 | 1）
    must_t(&mut c, &mut s, 0xB506); // push {r1, r2, lr}
    assert_eq!(c.reg(13), 0x4000 - 12);
    for (i, want) in [0x11, 0x22, 0x1235].into_iter().enumerate() {
        assert_eq!(s.r(0x4000 - 12 + 4 * i as u32, 4), want);
    }
    c.set_reg(1, 0);
    c.set_reg(2, 0);
    must_t(&mut c, &mut s, 0xBD06); // pop {r1, r2, pc}
    assert_eq!((c.reg(1), c.reg(2), c.reg(13)), (0x11, 0x22, 0x4000));
    // v4T: POP {pc} は状態を変えない（bit0 は無視）
    assert!(c.pc() == 0x1234 && c.thumb());
}

#[test]
fn thumb_ldm_stm() {
    let (mut c, mut s) = new_thumb();
    c.set_reg(0, 0x3000);
    c.set_reg(1, 0xAA);
    c.set_reg(2, 0xBB);
    must_t(&mut c, &mut s, 0xC006); // stmia r0!, {r1, r2}
    assert_eq!(c.reg(0), 0x3008);
    assert_eq!(s.r(0x3000, 4), 0xAA);
    c.set_reg(0, 0x3000);
    c.set_reg(1, 0);
    c.set_reg(2, 0);
    must_t(&mut c, &mut s, 0xC806); // ldmia r0!, {r1, r2}
    assert_eq!((c.reg(1), c.reg(2), c.reg(0)), (0xAA, 0xBB, 0x3008));
}

#[test]
fn thumb_cond_branch() {
    let (mut c, mut s) = new_thumb();
    set_flags(&mut c, "Z");
    must_t(&mut c, &mut s, 0xD003); // beq +6
    assert_eq!(c.pc(), TEST_PC + 4 + 6);
    // 不成立なら次の命令へ
    let (mut c, mut s) = new_thumb();
    must_t(&mut c, &mut s, 0xD003);
    assert_eq!(c.pc(), TEST_PC + 2);
    // 後方分岐
    let (mut c, mut s) = new_thumb();
    must_t(&mut c, &mut s, 0xD1FE); // bne -4 → PC+4-4 = PC
    assert_eq!(c.pc(), TEST_PC);
}

#[test]
fn thumb_uncond_branch() {
    let (mut c, mut s) = new_thumb();
    must_t(&mut c, &mut s, 0xE010); // b +32
    assert_eq!(c.pc(), TEST_PC + 4 + 32);
}

#[test]
fn thumb_bl() {
    let (mut c, mut s) = new_thumb();
    // BL +0x100: prefix(imm11=0) + suffix(imm11=0x80)
    must_t(&mut c, &mut s, 0xF000); // bl prefix: lr = PC+4 + 0
    must_t(&mut c, &mut s, 0xF880); // bl suffix: pc = lr + 0x100
    assert_eq!(c.pc(), TEST_PC + 4 + 0x100);
    // LR = suffix の次の命令 | 1
    assert_eq!(c.reg(14), (TEST_PC + 4) | 1);
    // 負のオフセット
    let (mut c, mut s) = new_thumb();
    must_t(&mut c, &mut s, 0xF7FF); // prefix: lr = PC+4 + (-1<<12)
    must_t(&mut c, &mut s, 0xFFFE); // suffix: → PC
    assert_eq!(c.pc(), TEST_PC + 4 - 4);
}

#[test]
fn thumb_swi() {
    let (mut c, mut s) = new_thumb();
    must_t(&mut c, &mut s, 0xDF10); // swi #0x10
    assert!(mode(&c) == MODE_SVC && !c.thumb());
    assert_eq!(c.pc(), VEC_SWI);
    assert_eq!(c.reg(14), TEST_PC + 2, "LR = SWI の次の Thumb 命令");
    // SPSR の T が立っているので MOVS pc, lr で Thumb に復帰できる
    assert_ne!(spsr(&c) & FLAG_T, 0);
}

// ---- run（ブロック実行）----

#[test]
fn run_stops_when_budget_lowered() {
    // 上限を実行中に下げられることを、budget を直接下げて確かめる
    // （machine の時間同期が使う経路）。NOP を 10 個並べる。
    let (mut c, mut s) = new_core();
    for i in 0..10 {
        s.w(TEST_PC + 4 * i, 4, 0xE1A00000);
    }
    assert_eq!(c.run(&mut s, 3), Ok(3));
    assert_eq!(c.pc(), TEST_PC + 12);
    s.run.limit(1);
    assert_eq!(s.run.budget, 1);
    s.run.limit(5); // 上げはしない
    assert_eq!(s.run.budget, 1);
}

// ---- ArchRegs（Go の archregs_test）----

/// arch_regs が現在モードに関係なく同じ値を返すこと（一致確認のダンプは
/// 内部の退避の持ち方に依存してはならないため）。
#[test]
fn arch_regs_independent_of_current_mode() {
    let (mut c, mut s) = new_core();
    let modes = [MODE_USR, MODE_FIQ, MODE_IRQ, MODE_SVC, MODE_ABT, MODE_UND];
    for &m in &modes {
        c.set_cpsr(m | FLAG_I | FLAG_F, &mut s);
        for r in 8..=14 {
            if r < 13 && m != MODE_USR && m != MODE_FIQ {
                continue; // r8〜r12 は FIQ 以外で共有（usr で書いた値を残す）
            }
            c.set_reg(r, m << 8 | r as u32);
        }
        if m != MODE_USR {
            let b = c.cur_bank();
            c.spsr[b] = 0x1000 | m;
        }
    }
    for r in 0..8 {
        c.set_reg(r, 0xA0 + r as u32);
    }
    let want = |m: u32, r: u32| {
        let m = if r < 13 && m != MODE_FIQ { MODE_USR } else { m };
        m << 8 | r
    };
    let mut first = None;
    for m in modes.into_iter().chain([MODE_SYS]) {
        c.set_cpsr(m | FLAG_I | FLAG_F, &mut s);
        let mut a = c.arch_regs();
        for r in 8..=14 {
            assert_eq!(a.usr[r as usize - 8], want(MODE_USR, r));
            assert_eq!(a.fiq[r as usize - 8], want(MODE_FIQ, r));
        }
        for (bm, v) in [
            (MODE_IRQ, a.irq),
            (MODE_SVC, a.svc),
            (MODE_ABT, a.abt),
            (MODE_UND, a.und),
        ] {
            assert_eq!(v, [want(bm, 13), want(bm, 14)], "mode {m:02X}");
        }
        for (j, bm) in [MODE_FIQ, MODE_IRQ, MODE_SVC, MODE_ABT, MODE_UND]
            .into_iter()
            .enumerate()
        {
            assert_eq!(a.spsr[j], 0x1000 | bm);
        }
        let cur = if m == MODE_SYS { MODE_USR } else { m };
        for r in 8..=14 {
            assert_eq!(a.r[r as usize], want(cur, r), "mode {m:02X} r{r}");
        }
        // モード固有の部分（R・CPSR）以外は全モードで同じ。
        a.r = [0; 16];
        a.cpsr = 0;
        match first {
            None => first = Some(a),
            Some(f) => assert_eq!(a, f, "mode {m:02X}"),
        }
    }
}

// ---- シフタ（Rust のシフトの場合分けの確認）----

/// シフト量のすべての値で、Rust の場合分けが Go の意味（32 以上で 0 等）と
/// 同じ結果になることを、u64 の素朴な計算と比べて確かめる。
#[test]
fn shift_reg_all_amounts() {
    for &v in &[
        0u32, 1, 0x80000000, 0x80000001, 0xFFFFFFFF, 0x12345678, 0xF000000F,
    ] {
        for amount in 0..=255u32 {
            for cin in [false, true] {
                let lsl = shift_reg(v, 0, amount, cin);
                let lsr = shift_reg(v, 1, amount, cin);
                let asr = shift_reg(v, 2, amount, cin);
                if amount == 0 {
                    assert_eq!([lsl, lsr, asr], [(v, cin); 3]);
                    continue;
                }
                let wide = (v as u64) << amount.min(40);
                assert_eq!(
                    lsl.0,
                    if amount >= 32 { 0 } else { wide as u32 },
                    "lsl {v:X} {amount}"
                );
                assert_eq!(
                    lsl.1,
                    amount <= 32 && (wide >> 32) & 1 != 0,
                    "lsl c {v:X} {amount}"
                );
                assert_eq!(lsr.0, if amount >= 32 { 0 } else { v >> amount });
                assert_eq!(lsr.1, amount <= 32 && (v as u64) >> (amount - 1) & 1 != 0);
                let sa = ((v as i32 as i64) >> amount.min(40)) as u32;
                assert_eq!(asr.0, sa, "asr {v:X} {amount}");
                assert_eq!(asr.1, (v as i32 as i64) >> (amount.min(40) - 1) & 1 != 0);
            }
        }
    }
}

// ---- 乱数（テスト用）----

/// splitmix64（テストの入力を決まった列で作る）。
struct SplitMix(u64);

impl SplitMix {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E3779B97F4A7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
        z ^ (z >> 31)
    }
    fn u32(&mut self) -> u32 {
        (self.next() >> 32) as u32
    }
    fn choose(&mut self, n: u32) -> u32 {
        self.u32() % n
    }
}

// ---- 特化（Go の special_test）----

/// 特化した実行関数（special.rs）が汎用の実行関数と同じ結果になることを、
/// ランダムな命令語とランダムな状態（レジスタ・フラグ・メモリ）で確かめる。
/// 命令語は特化の対象になる空間（データ処理・LDR/STR 即値・B/BL）から作り、
/// 特化されなかったものは数えるだけにする。
#[test]
fn specialized_matches_generic() {
    let mut rng = SplitMix(1);
    const MEM: u32 = 16 * 1024;
    let mut special = 0;
    for _ in 0..100_000 {
        let word = match rng.choose(4) {
            0 => 0xE2000000 | rng.u32() & 0x01FFFFFF, // データ処理（即値）
            1 => 0xE0000000 | rng.u32() & 0x01FFF00F, // データ処理（レジスタ・シフトなし）
            2 => 0xE4000000 | rng.u32() & 0x01FFFFFF, // LDR/STR 即値
            _ => 0xEA000000 | rng.u32() & 0x01FFFFFF, // B/BL
        };
        let Some(sp) = super::special::specialize::<TestSys>(word) else {
            continue;
        };
        special += 1;
        // 同じ初期状態のコアを 2 つ作る。アドレスがメモリ内に収まるよう、
        // レジスタは小さい値にする（LDR/STR のベース）。
        let mut regs = [0u32; 16];
        for r in regs.iter_mut() {
            *r = if rng.choose(4) == 0 {
                rng.u32()
            } else {
                rng.u32() % MEM
            };
        }
        let flags = rng.u32() & (FLAG_N | FLAG_Z | FLAG_C | FLAG_V);
        let mem: Vec<u8> = (0..MEM).map(|_| rng.u32() as u8).collect();
        let run = |f: ExecFn<TestSys>, imm: u32| {
            let mut s = TestSys::new();
            s.mem = mem.clone();
            s.run.budget = 10;
            let mut c = Cpu::new();
            c.regs = regs;
            c.regs[15] = 0x1000 + 4; // 実行中は PC+4
            c.cpsr = MODE_SVC | flags;
            let r = f(&mut c, &mut s, word, imm);
            (c, s, r)
        };
        let (a, am, ar) = run(sp.exec, sp.imm);
        let (b, bm, br) = run(decode::<TestSys>(word), 0);
        assert_eq!(
            ar.is_ok(),
            br.is_ok(),
            "{word:08X}: error mismatch {ar:?} {br:?}"
        );
        if ar.is_err() {
            continue; // 範囲外アクセス: どちらもエラーならよい
        }
        assert_eq!(
            (a.regs, a.cpsr, a.spin_hint),
            (b.regs, b.cpsr, b.spin_hint),
            "{word:08X}: state"
        );
        assert_eq!(am.run.budget, bm.run.budget, "{word:08X}: run budget");
        assert!(am.mem == bm.mem, "{word:08X}: memory");
    }
    assert!(special >= 25000, "only {special} specialized words tested");
}
