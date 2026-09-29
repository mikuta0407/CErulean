//! デコードキャッシュ（性能対策。ユーザー確認済み 2026-09。Go の codecache.go）。
//!
//! 物理 RAM ページ（4KB）ごとに、デコード済みの ARM 命令（[`Instr`]）の表を持つ。
//! 実行中のページとその仮想ページを覚えておき、PC が同じページにあれば TLB を
//! 引かず表から命令を取る。デコードは命令語だけの純関数なので、物理ページ単位で
//! 共有してよい。
//!
//! 所有権の設計（計画書 §3.3 の案 A）: キャッシュは CPU（Cpu）ではなくシステム
//! （sys）が持つ。実行中の仮想ページ（Go の curVA）は MMU が持ち、MMU が世代を
//! 上げる・コードページに書き込まれるとその場で無効にする（Go の SetGenHook の
//! 代わり）。コードページへの書き込みで捨てるべきページは MMU が記録し、次に
//! ページに入るとき（遅い経路）にここで捨てる。
//!
//! 正しさ（1 命令ずつフェッチ・デコードした場合と全状態が一致すること）:
//!   - 変換: MMU は TLB の全無効化・エントリの詰め替え・特権状態や FCSE PID の
//!     変化で世代を上げる（mmu.rs の「デコードキャッシュの支援」）。世代が同じ間は、
//!     同じ VA のフェッチは同じ TLB エントリにヒットし、TLB の状態も変えない
//!     （= 省略してよい）。ページに入るとき（enter）は TLB ヒットのときだけ
//!     キャッシュを使い、ミスなら通常のフェッチで TLB を埋める。
//!   - 書き換え: デコードしたページは MMU に印を付け（mark_code）、そのページへの
//!     書き込みで捨てる。次のフェッチは書き換え後の命令語を読み直す（自己書き換え
//!     コード・DLL のロードで同じ物理ページが再利用される場合も含む）。
//!   - Thumb は対象外（1 命令ずつ読む。WM5 の実行中にはほぼ現れない）。
//!
//! ここで持つものはすべて派生情報で、スナップショットには保存しない。

use std::collections::HashMap;

use super::ir::{Instr, decode_instr};
use crate::bus::RamOff;

/// デコードキャッシュに必要な MMU とメモリの機能（ボードが実装する）。
pub trait CodeMemory {
    /// va を含むページの物理先頭と RAM の位置を返す。TLB ヒットで RAM のときだけ
    /// Some で、TLB の状態を変えない。
    fn code_page(&mut self, va: u32) -> Option<(u32, RamOff)>;
    /// 変換の世代番号。
    fn code_gen(&self) -> u64;
    /// 物理ページ pa をコードとして書き込み検出の対象にする。
    fn mark_code(&mut self, pa: u32);
    /// 書き込みでデコード結果を捨てるべき物理ページを 1 つ取り出す。
    fn take_code_invalidated(&mut self) -> Option<u32>;
    /// 実行中の仮想ページを設定する（無効にするときは 4KB 境界でない値）。
    fn set_code_cur_va(&mut self, va: u32);
    /// RAM の位置 off のワード。
    fn ram_word(&self, off: RamOff) -> u32;
}

/// 物理 4KB ページ 1 枚分のデコード済み ARM 命令。
struct CodePage {
    pa: u32,
    ram: RamOff,
    /// None は未デコード
    arm: Box<[Option<Instr>; 1024]>,
    /// MMU に mark_code 済み（書き込みで外れる）
    marked: bool,
}

/// 仮想ページ → デコード済みページの対応（世代つき）。関数呼び出しなどで
/// ページをまたぐたびに TLB とマップを引かないためのもの。同じ世代の間は、
/// その仮想ページの TLB エントリが残っている（CPU が覚えたエントリの詰め替えで
/// 世代が上がる）ので、code_page を呼んだのと同じ結果になる。
#[derive(Clone, Copy)]
struct VpageEnt {
    /// 仮想ページ先頭（無効なら 4KB 境界でない値）
    va: u32,
    generation: u64,
    page: u32,
}

const VPAGE_BITS: u32 = 6;

/// デコードキャッシュ。
pub struct CodeCache {
    /// 物理ページ番号 → pages の添字
    index: HashMap<u32, u32>,
    pages: Vec<CodePage>,
    /// 実行中のページ（pages の添字）
    cur: u32,
    vpages: [VpageEnt; 1 << VPAGE_BITS],
}

impl Default for CodeCache {
    fn default() -> Self {
        Self::new()
    }
}

impl CodeCache {
    pub fn new() -> Self {
        CodeCache {
            index: HashMap::new(),
            pages: vec![],
            cur: 0,
            vpages: [VpageEnt {
                va: 1,
                generation: 0,
                page: 0,
            }; 1 << VPAGE_BITS],
        }
    }

    /// デコードキャッシュを空にする（リセット・スナップショット復元時。MMU 側も
    /// 印を全部外すこと）。
    pub fn reset(&mut self) {
        *self = Self::new();
    }

    /// 実行中のページ（呼び出し側が PC の仮想ページが MMU の code_cur_va と
    /// 一致することを確かめた後）から命令を取る。未デコードならデコードする。
    #[inline(always)]
    pub fn cur_instr(&mut self, pc: u32, mem: &mut impl CodeMemory) -> Instr {
        let p = &self.pages[self.cur as usize];
        match &p.arm[((pc >> 2) & 0x3FF) as usize] {
            Some(i) => *i,
            None => self.decode_cached(pc, mem),
        }
    }

    /// PC が別のページに移った（または世代が変わった）ときに、そのページの
    /// キャッシュを引き当てて命令を返す。キャッシュできなければ None（呼び出し側が
    /// 通常のフェッチをする。TLB ミスならそこで TLB が埋まる）。
    pub fn enter(&mut self, pc: u32, mem: &mut impl CodeMemory) -> Option<Instr> {
        // コードページへの書き込みがあったページのデコード結果を捨てる。
        while let Some(pa) = mem.take_code_invalidated() {
            if let Some(&i) = self.index.get(&(pa >> 12)) {
                let p = &mut self.pages[i as usize];
                p.arm.fill(None);
                p.marked = false;
            }
        }
        let va = pc & !0xFFF;
        let generation = mem.code_gen();
        let v = &mut self.vpages[((pc >> 12) & ((1 << VPAGE_BITS) - 1)) as usize];
        let page = if v.va == va && v.generation == generation {
            v.page
        } else {
            let Some((pa, ram)) = mem.code_page(pc) else {
                mem.set_code_cur_va(1);
                return None;
            };
            let page = match self.index.get(&(pa >> 12)) {
                Some(&i) => i,
                None => {
                    let i = self.pages.len() as u32;
                    self.pages.push(CodePage {
                        pa,
                        ram,
                        arm: Box::new([None; 1024]),
                        marked: false,
                    });
                    self.index.insert(pa >> 12, i);
                    i
                }
            };
            // code_page は MMU の世代を変えない（watched の印を付けるだけ）。
            *v = VpageEnt {
                va,
                generation,
                page,
            };
            page
        };
        self.cur = page;
        mem.set_code_cur_va(va);
        Some(self.cur_instr(pc, mem))
    }

    /// 実行中のページの pc の命令をデコードして表に入れる。
    fn decode_cached(&mut self, pc: u32, mem: &mut impl CodeMemory) -> Instr {
        let p = &mut self.pages[self.cur as usize];
        if !p.marked {
            // 表に載せる前に書き込み検出を有効にする。
            mem.mark_code(p.pa);
            p.marked = true;
        }
        let i = decode_instr(mem.ram_word(p.ram + (pc & 0xFFC)));
        p.arm[((pc >> 2) & 0x3FF) as usize] = Some(i);
        i
    }

    /// デコード済みのページ数（計測用。wasm のメモリ予算の検討に使う）。
    pub fn page_count(&self) -> usize {
        self.pages.len()
    }
}
