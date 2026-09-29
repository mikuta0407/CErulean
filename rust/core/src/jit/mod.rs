//! JIT-to-wasm（段階5。設計は docs/stage5-design.md、2026-09-29 ユーザー確認済み）。
//!
//! よく実行されるブロック（ページ内の直線の命令列。codegen.rs）を wasm の関数に変換し、
//! ホスト（web クレートの [`JitHost`]）が本体と同じ線形メモリを import した
//! モジュールとして読み込む。コアは生成（IR → wasm のバイト列）と管理（実行回数・
//! 無効化・上限）だけを持ち、プラットフォーム非依存のまま（ネイティブではホストが
//! なく、インタプリタだけで動く）。
//!
//! 正しさ（インタプリタと全状態が完全一致すること）:
//!   - 生成コードは扱えないこと（TLB ミス・MMIO・コードページへの書き込みなど）の
//!     手前で戻り、インタプリタがその命令から続ける（codegen.rs の約束）。
//!   - 生成コードに入るのは、インタプリタのブロック実行（run_page）に入れる
//!     ときと同じ条件のとき（履歴なし・ARM 状態・割り込みなし・PC が実行中の
//!     デコード済みページにあり変換も変わっていない）だけ。ブロックの間は
//!     これらが変わらない（codegen.rs）。
//!   - ブロックは物理ページの命令番号で引く（デコードキャッシュのページに枠を
//!     持つ）。ページへの書き込みでデコード結果を捨てるときに枠も捨てる。
//!   - JIT の有無・コンパイルの時期はゲストの状態に影響しない。ここで持つものは
//!     すべて派生情報で、スナップショットに保存しない。

mod codegen;
pub mod selftest;
mod wasm;

use crate::arm::{CodeCache, CodeMemory, Cpu, JitRun, RunCtl};
use crate::mmu::Mmu;

/// 生成したモジュールの読み込みと呼び出し（web クレートが実装する）。
pub trait JitHost {
    /// wasm のバイト列（export "0".."nfuncs-1"）を読み込み、関数の番号の先頭を返す
    /// （i 番目の関数は 先頭 + i）。
    fn load(&mut self, wasm: &[u8], nfuncs: u32) -> Result<u32, String>;
    /// 関数 func を ctx（[`JitCtx`] のアドレス）で呼び、戻り値を返す。
    fn call(&mut self, func: u32, ctx: u32) -> u32;
    /// 読み込んだ関数をすべて捨てる（番号は 0 から振り直す）。
    fn release_all(&mut self);
}

/// 生成コードとの取り決め（呼ぶ直前に Rust が埋める。アドレスは wasm32 の線形
/// メモリ上の位置なので、ホストのある wasm32 でだけ意味がある）。
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct JitCtx {
    /// Cpu::regs の先頭
    pub regs: u32,
    /// Cpu::cpsr
    pub cpsr: u32,
    /// ソフト TLB の先頭
    pub tlb: u32,
    /// RAM のアリーナの先頭
    pub arena: u32,
    /// FCSE PID（c13 の値。bits 31:25）
    pub pid: u32,
    /// 現在の特権状態で見る TLB の権限ビット（読み・書き）
    pub perm_r: u32,
    pub perm_w: u32,
    /// 上限までの残りの命令数（u32 に切り詰めたもの）
    pub remaining: u32,
}

// デコードキャッシュのページの枠（命令ごとの u32）の値:
//   0〜QUEUED-1: 未コンパイル（ブロックの先頭として入った回数）
//   QUEUED: コンパイル待ち
//   NEVER: 先頭の命令が対象外（数えない）
//   COMPILED | 関数の番号: コンパイル済み
const QUEUED: u32 = 0x4000_0000;
const NEVER: u32 = 0x4000_0001;
const COMPILED: u32 = 0x8000_0000;

/// 読み込んだコードの合計の上限（バイト数）。超えたら全部捨てて作り直す。
/// TODO: iOS Safari のメモリ予算を計測して決める（段階5-4）。
const CODE_LIMIT: u64 = 32 << 20;

/// 計測・試験用の数。
#[derive(Clone, Debug, Default)]
pub struct JitStats {
    /// コンパイルしたブロック・モジュール・バイト数
    pub blocks: u64,
    pub modules: u64,
    pub bytes: u64,
    /// ブロックに入った回数・その中で実行した命令数・サイド出口の回数
    pub calls: u64,
    pub executed: u64,
    pub side_exits: u64,
    /// 上限で全部捨てた回数
    pub flushes: u64,
    /// コンパイルしたブロックに含まれた Op ごとの数（試験で全 Op を試したかを見る）
    pub ops: Vec<u64>,
}

/// JIT の管理。
pub struct Jit {
    host: Option<Box<dyn JitHost>>,
    /// ブロックの先頭として何回入ったらコンパイルするか（1 以上）
    threshold: u32,
    /// 何ブロックたまったら 1 モジュールにするか（1 以上）
    batch: usize,
    /// コンパイル待ち（ページの添字, 命令番号）
    queue: Vec<(u32, u32)>,
    ctx: JitCtx,
    /// 今読み込んでいるコードのバイト数
    loaded: u64,
    stats: JitStats,
    /// 読み込みに失敗して無効にした理由
    error: Option<String>,
}

impl Default for Jit {
    fn default() -> Self {
        Self::new()
    }
}

impl Jit {
    pub fn new() -> Jit {
        Jit {
            host: None,
            threshold: 64,
            batch: 32,
            queue: vec![],
            ctx: JitCtx::default(),
            loaded: 0,
            stats: JitStats::default(),
            error: None,
        }
    }

    /// ホストを設定する（None で無効）。threshold・batch は発火条件（0 は 1 とみなす）。
    /// 呼び出し側（Machine）はデコードキャッシュの枠を捨ててから呼ぶこと。
    pub(crate) fn set_host(&mut self, host: Option<Box<dyn JitHost>>, threshold: u32, batch: u32) {
        if let Some(h) = &mut self.host {
            h.release_all();
        }
        self.host = host;
        self.threshold = threshold.clamp(1, QUEUED - 1);
        self.batch = batch.max(1) as usize;
        self.queue.clear();
        self.loaded = 0;
        self.error = None;
    }

    #[inline(always)]
    pub(crate) fn enabled(&self) -> bool {
        self.host.is_some()
    }

    pub fn stats(&self) -> &JitStats {
        &self.stats
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// 読み込んだ関数とコンパイル待ちを全部捨てる（デコードキャッシュの枠は
    /// 呼び出し側が捨てる。リセット・スナップショットの読み込み・上限）。
    pub(crate) fn flush(&mut self, code: &mut CodeCache) {
        if let Some(h) = &mut self.host {
            h.release_all();
        }
        code.clear_jit();
        self.queue.clear();
        self.loaded = 0;
    }

    /// ブロックの先頭（PC が実行中のページにある）で呼ぶ。コンパイル済みなら
    /// 実行して Ran を返す。未コンパイルなら数え、閾値に達したら待ち行列に積む
    /// （たまったらコンパイルする）。
    pub(crate) fn enter(
        &mut self,
        cpu: &mut Cpu,
        mmu: &Mmu,
        arena: &mut [u8],
        run: &mut RunCtl,
        code: &mut CodeCache,
    ) -> Action {
        let pc = cpu.regs[15];
        let slot = code.jit_slot(pc);
        let v = *slot;
        if v & COMPILED != 0 {
            return Action::Ran(self.call(v & !COMPILED, cpu, mmu, arena, run));
        }
        if v >= QUEUED {
            return Action::Ran(JitRun::No);
        }
        if v + 1 < self.threshold {
            *slot = v + 1;
            return Action::Ran(JitRun::No);
        }
        *slot = QUEUED;
        self.queue.push((code.cur_page(), (pc >> 2) & 0x3FF));
        if self.queue.len() >= self.batch {
            Action::Compile
        } else {
            Action::Ran(JitRun::No)
        }
    }

    /// コンパイル待ちがあるか。
    pub(crate) fn pending(&self) -> bool {
        !self.queue.is_empty()
    }

    /// 生成関数を呼ぶ。
    fn call(
        &mut self,
        func: u32,
        cpu: &mut Cpu,
        mmu: &Mmu,
        arena: &mut [u8],
        run: &mut RunCtl,
    ) -> JitRun {
        let Some(host) = &mut self.host else {
            return JitRun::No;
        };
        let (perm_r, perm_w) = mmu.jit_perms();
        // 生成コードは、ここで渡すアドレス（呼ぶ直前に &mut から作ったもの）を
        // 通してだけ cpu.regs・cpu.cpsr・アリーナを書き換え、TLB を読む。呼び出しの
        // 間 Rust の側はこれらの参照を使わない。書き換えの正しさは差分テスト
        // （selftest）と基準シナリオで確かめる。
        self.ctx = JitCtx {
            regs: addr(cpu.regs.as_mut_ptr()),
            cpsr: addr(&raw mut cpu.cpsr),
            tlb: addr(mmu.tlb.as_ptr()),
            arena: addr(arena.as_mut_ptr()),
            pid: mmu.pid,
            perm_r: perm_r as u32,
            perm_w: perm_w as u32,
            remaining: run.budget.saturating_sub(run.n).min(u32::MAX as u64) as u32,
        };
        let r = host.call(func, addr(&self.ctx));
        let executed = (r & 0xFFFF) as u64;
        let side = r & codegen::SIDE != 0;
        run.n += executed;
        self.stats.calls += 1;
        self.stats.executed += executed;
        self.stats.side_exits += side as u64;
        JitRun::Ran {
            resume: side || executed == 0,
        }
    }

    /// コンパイル待ちのブロックをまとめて 1 モジュールにし、読み込む。
    pub(crate) fn compile(&mut self, code: &mut CodeCache, mem: &mut impl CodeMemory) {
        if self.host.is_none() {
            self.queue.clear();
            return;
        }
        // 書き込まれたページのデコード結果（と枠）を先に捨てる（待ち行列の中の
        // そのページのブロックは枠が QUEUED でなくなるので飛ばされる）。
        code.drain_invalidated(mem);
        let mut funcs = vec![];
        let mut targets = vec![];
        for (page, idx) in std::mem::take(&mut self.queue) {
            if code.slot(page, idx) != QUEUED {
                continue;
            }
            let block = codegen::form_block(idx, |i| code.instr_at(page, i, mem));
            if block.is_empty() {
                code.set_slot(page, idx, NEVER);
                continue;
            }
            if self.stats.ops.is_empty() {
                self.stats.ops = vec![0; 256];
            }
            for i in &block {
                self.stats.ops[i.op as usize] += 1;
            }
            funcs.push(codegen::gen_block(&block));
            targets.push((page, idx));
        }
        if funcs.is_empty() {
            return;
        }
        let bytes = wasm::module(&funcs);
        if self.loaded + bytes.len() as u64 > CODE_LIMIT {
            self.flush(code);
            self.stats.flushes += 1;
        }
        let Some(host) = &mut self.host else {
            return;
        };
        match host.load(&bytes, funcs.len() as u32) {
            Ok(base) => {
                for (n, (page, idx)) in targets.into_iter().enumerate() {
                    code.set_slot(page, idx, COMPILED | (base + n as u32));
                }
                self.loaded += bytes.len() as u64;
                self.stats.blocks += funcs.len() as u64;
                self.stats.modules += 1;
                self.stats.bytes += bytes.len() as u64;
            }
            Err(e) => {
                // 生成の誤り（またはホストの制約）。インタプリタに戻す。
                self.error = Some(e);
                host.release_all();
                self.host = None;
                code.clear_jit();
            }
        }
    }
}

/// enter の結果。
pub(crate) enum Action {
    Ran(JitRun),
    /// 待ち行列がたまった（呼び出し側がデコードキャッシュのメモリを渡して compile を呼ぶ）
    Compile,
}

/// 線形メモリ上のアドレス（wasm32 でだけ意味がある）。
fn addr<T>(p: *const T) -> u32 {
    p as usize as u32
}
