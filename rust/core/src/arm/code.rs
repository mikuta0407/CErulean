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

/// JIT の枠（命令ごと。値の意味は jit/mod.rs）。CodePage と別の表に置くのは、
/// インタプリタが毎命令引く CodePage を小さく保つため（wasm で JIT なしの速度が
/// 変わらないように）。
type JitSlots = Option<Box<[u32; 1024]>>;

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
    /// pages と同じ添字の JIT の枠（JIT を使うページだけ作る）
    jit: Vec<JitSlots>,
    /// pages と同じ添字の、ページの関数を作った回数（JIT の作り直しを間引く。jit/mod.rs）
    jit_level: Vec<u8>,
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
            jit: vec![],
            jit_level: vec![],
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
    // wasm では実行ループに展開する（JIT を足す前の wasm のビルドと同じ形。展開
    // されないと JIT なしで約 1 割遅かった。2026-09-29、Node で計測）。
    #[cfg_attr(target_arch = "wasm32", inline(always))]
    pub fn enter(&mut self, pc: u32, mem: &mut impl CodeMemory) -> Option<Instr> {
        // コードページへの書き込みがあったページのデコード結果（と JIT の枠）を捨てる。
        // drain_invalidated と同じ処理だが、ここは実行ループに展開されるように
        // 関数呼び出しにしない（wasm で呼び出しにすると JIT なしの速度が落ちた）。
        while let Some(pa) = mem.take_code_invalidated() {
            self.invalidate(pa);
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
                    self.jit.push(None);
                    self.jit_level.push(0);
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
        self.decode_at(self.cur, (pc >> 2) & 0x3FF, mem)
    }

    /// ページ page の命令番号 idx をデコードして表に入れる。
    fn decode_at(&mut self, page: u32, idx: u32, mem: &mut impl CodeMemory) -> Instr {
        let p = &mut self.pages[page as usize];
        if !p.marked {
            // 表に載せる前に書き込み検出を有効にする。
            mem.mark_code(p.pa);
            p.marked = true;
        }
        let i = decode_instr(mem.ram_word(p.ram + idx * 4));
        p.arm[idx as usize] = Some(i);
        i
    }

    /// コードページへの書き込みがあったページのデコード結果（と JIT の枠）を捨てる。
    pub fn drain_invalidated(&mut self, mem: &mut impl CodeMemory) {
        while let Some(pa) = mem.take_code_invalidated() {
            self.invalidate(pa);
        }
    }

    /// 物理ページ pa のデコード結果と JIT の枠を捨てる（書き込みは稀なので呼び出しでよい）。
    #[inline(never)]
    fn invalidate(&mut self, pa: u32) {
        if let Some(&i) = self.index.get(&(pa >> 12)) {
            let p = &mut self.pages[i as usize];
            p.arm.fill(None);
            p.marked = false;
            if let Some(s) = &mut self.jit[i as usize] {
                s.fill(0);
            }
            self.jit_level[i as usize] = 0;
        }
    }

    // ---- JIT の支援（jit/mod.rs）----

    /// 実行中のページの添字。
    pub fn cur_page(&self) -> u32 {
        self.cur
    }

    /// 実行中のページの pc の JIT の枠（呼び出し側が PC が実行中のページにあることを
    /// 確かめた後）。
    #[inline(always)]
    pub fn jit_slot(&mut self, pc: u32) -> &mut u32 {
        let s = &mut self.jit[self.cur as usize];
        &mut s.get_or_insert_with(|| Box::new([0; 1024]))[((pc >> 2) & 0x3FF) as usize]
    }

    /// ページ page の関数を作った回数。
    pub fn jit_level(&self, page: u32) -> u8 {
        self.jit_level[page as usize]
    }

    pub fn bump_jit_level(&mut self, page: u32) {
        let l = &mut self.jit_level[page as usize];
        *l = l.saturating_add(1);
    }

    /// ページ page の命令番号 idx がデコード済み（一度は実行された）か。
    pub fn is_decoded(&self, page: u32, idx: u32) -> bool {
        self.pages[page as usize].arm[idx as usize].is_some()
    }

    /// ページ page の命令番号 idx の JIT の枠（なければ 0）。
    pub fn slot(&self, page: u32, idx: u32) -> u32 {
        self.jit[page as usize]
            .as_ref()
            .map_or(0, |s| s[idx as usize])
    }

    pub fn set_slot(&mut self, page: u32, idx: u32, v: u32) {
        self.jit[page as usize].get_or_insert_with(|| Box::new([0; 1024]))[idx as usize] = v;
    }

    /// ページ page の命令番号 idx の命令（未デコードならデコードする）。
    pub fn instr_at(&mut self, page: u32, idx: u32, mem: &mut impl CodeMemory) -> Instr {
        match self.pages[page as usize].arm[idx as usize] {
            Some(i) => i,
            None => self.decode_at(page, idx, mem),
        }
    }

    /// JIT の枠をすべて捨てる。
    pub fn clear_jit(&mut self) {
        self.jit.iter_mut().for_each(|s| *s = None);
        self.jit_level.fill(0);
    }

    /// デコード済みのページ数（計測用。wasm のメモリ予算の検討に使う）。
    pub fn page_count(&self) -> usize {
        self.pages.len()
    }
}
