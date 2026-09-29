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

use crate::arm::{CodeCache, CodeMemory, Cpu, Instr, JitRun, RunCtl};
use crate::mmu::Mmu;

/// 生成したモジュールの読み込みと呼び出し（web クレートが実装する）。
pub trait JitHost {
    /// wasm のバイト列（export "0".."nfuncs-1"）を読み込み、関数の番号の先頭を返す
    /// （i 番目の関数は 先頭 + i）。
    fn load(&mut self, wasm: &[u8], nfuncs: u32) -> Result<u32, String>;
    /// 関数 func を ctx（[`JitCtx`] のアドレス）で呼び、戻り値を返す。
    fn call(&mut self, func: u32, ctx: u32) -> u32;
    /// 関数 func を捨てる（番号は再利用しない）。
    fn release(&mut self, func: u32);
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
    /// 出力: 実行した命令数
    pub executed: u32,
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

/// 生成関数 1 つの命令列の大きさの目安の上限（バイト数。ブロック 1 つがこれを超える
/// ときはそのブロックだけの関数にする）。
const FUNC_LIMIT: usize = 8 << 10;

/// 計測・試験用の数。
#[derive(Clone, Debug, Default)]
pub struct JitStats {
    /// 生成したブロックの数（ページの作り直しで同じブロックを何度も数える）
    pub blocks: u64,
    /// 作った関数（ページ単位。大きさの上限で分けたもの・作り直しを含む）・モジュール・
    /// バイト数の累計
    pub pages: u64,
    pub modules: u64,
    pub bytes: u64,
    /// 作った関数の本体の最大のバイト数（V8 の最適化コンパイルの作業領域は関数の
    /// 大きさで決まるので、メモリの目安にする。段階5-4）
    pub max_func: u64,
    /// ブロックに入った回数・その中で実行した命令数・サイド出口の回数
    pub calls: u64,
    pub executed: u64,
    pub side_exits: u64,
    /// 上限で全部捨てた回数
    pub flushes: u64,
    /// コンパイルしたブロックに含まれた Op ごとの数（試験で全 Op を試したかを見る）
    pub ops: Vec<u64>,
    /// サイド出口でない戻りのうち、ページを出た・Thumb に切り替わった・次の PC が
    /// ブロックの先頭でなかった回数（ブロックの連結が切れた理由）
    pub exit_page: u64,
    pub exit_thumb: u64,
    pub exit_other: u64,
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
    /// 前回のコンパイルの後に、コンパイル待ちのブロックに入った回数
    queued_hits: u32,
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
            queued_hits: 0,
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
        if v == QUEUED {
            // コンパイル待ちのブロックに何度も入るなら、batch に満たなくてもすぐ
            // コンパイルする（ループがコンパイル待ちのまま解釈され続けないように）。
            self.queued_hits += 1;
            if self.queued_hits >= self.threshold {
                return Action::Compile;
            }
            return Action::Ran(JitRun::No);
        }
        if v >= QUEUED {
            return Action::Ran(JitRun::No);
        }
        // 関数を作ったことのあるページの新しい入口は、作り直すたびに 4 倍入るまで
        // 待つ（まれにしか通らない入口のたびにページ全体を作り直すと、生成コードが
        // 膨らみ V8 のメモリが増え続けた。2026-09-29 の計測）。
        let page = code.cur_page();
        let level = code.jit_level(page).min(10) as u32;
        let need = self
            .threshold
            .saturating_mul(1 << (2 * level))
            .min(QUEUED - 1);
        let slot = code.jit_slot(pc);
        if v + 1 < need {
            *slot = v + 1;
            return Action::Ran(JitRun::No);
        }
        *slot = QUEUED;
        self.queue.push((page, (pc >> 2) & 0x3FF));
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
            executed: 0,
        };
        // ctx は生成コードが executed を書くので、&mut から作ったアドレスを渡す。
        let pc0 = cpu.regs[15];
        let side = host.call(func, addr(&raw mut self.ctx)) != 0;
        if !side {
            if (cpu.regs[15] ^ pc0) & !0xFFF != 0 {
                self.stats.exit_page += 1;
            } else if cpu.cpsr & crate::arm::FLAG_T != 0 {
                self.stats.exit_thumb += 1;
            } else {
                self.stats.exit_other += 1;
            }
        }
        let executed = self.ctx.executed as u64;
        run.n += executed;
        self.stats.calls += 1;
        self.stats.executed += executed;
        self.stats.side_exits += side as u64;
        JitRun::Ran {
            resume: side || executed == 0,
        }
    }

    /// コンパイル待ちのブロックのあるページごとに、そのページのコンパイル済みの
    /// ブロックも含めた関数を作り直し（ブロックを連結するため。codegen.rs）、
    /// まとめて 1 モジュールにして読み込む。古い関数は捨てる。
    pub(crate) fn compile(&mut self, code: &mut CodeCache, mem: &mut impl CodeMemory) {
        if self.host.is_none() {
            self.queue.clear();
            return;
        }
        // 書き込まれたページのデコード結果（と枠）を先に捨てる（待ち行列の中の
        // そのページのブロックは枠が QUEUED でなくなるので飛ばされる）。
        code.drain_invalidated(mem);
        self.queued_hits = 0;
        let mut pages: Vec<u32> = std::mem::take(&mut self.queue)
            .into_iter()
            .filter(|&(page, idx)| code.slot(page, idx) == QUEUED)
            .map(|(page, _)| page)
            .collect();
        pages.sort_unstable();
        pages.dedup();
        let mut funcs = vec![];
        // (ページ, ブロックの先頭の命令番号の列, 古い関数の番号の列)
        let mut targets = vec![];
        for page in pages {
            let mut blocks = vec![];
            let mut old = vec![];
            // 関数に入れるブロックの先頭（コンパイル待ちとコンパイル済み。先頭が対象外の
            // ものは NEVER にして外す）
            let mut entry = vec![false; 1024];
            for idx in 0..1024 {
                let v = code.slot(page, idx);
                if v & COMPILED != 0 {
                    old.push(v & !COMPILED);
                } else if v != QUEUED {
                    continue;
                }
                if codegen::supported(&code.instr_at(page, idx, mem)) {
                    entry[idx as usize] = true;
                } else {
                    code.set_slot(page, idx, NEVER);
                }
            }
            // 入口から静的にたどれる同じページ内の続き（分岐先・条件分岐と BL の次・
            // 対象外の命令の次）も入口に加える（後で 1 つずつ熱くなるたびに作り直さない
            // ように）。一度も実行されていない命令（デコードされていない）は加えない。
            let mut work: Vec<u32> = (0..1024u32).filter(|&i| entry[i as usize]).collect();
            while let Some(s) = work.pop() {
                let block = codegen::form_block(s, |i| code.instr_at(page, i, mem), |_| false);
                for n in codegen::successors(s, &block) {
                    if n < 1024
                        && !entry[n as usize]
                        && code.is_decoded(page, n)
                        && code.slot(page, n) != NEVER
                        && codegen::supported(&code.instr_at(page, n, mem))
                    {
                        entry[n as usize] = true;
                        work.push(n);
                    }
                }
            }
            for idx in 0..1024u32 {
                if !entry[idx as usize] {
                    continue;
                }
                let block = codegen::form_block(
                    idx,
                    |i| code.instr_at(page, i, mem),
                    |i| entry[i as usize],
                );
                if self.stats.ops.is_empty() {
                    self.stats.ops = vec![0; 256];
                }
                for i in &block {
                    self.stats.ops[i.op as usize] += 1;
                }
                blocks.push((idx, block));
            }
            if blocks.is_empty() {
                continue;
            }
            old.sort_unstable();
            old.dedup();
            code.bump_jit_level(page);
            self.stats.blocks += blocks.len() as u64;
            // 関数の大きさに上限を設けて、ページのブロックを先頭の命令番号の順に
            // 分ける（V8 の最適化コンパイルの作業領域は関数の大きさとともに急に増え、
            // 大きなページ関数で RSS が数百 MB 増えた。段階5-4 の計測）。別の関数の
            // ブロックへは Rust に戻ってから入り直す。大きさはブロックの本体の
            // 大きさの和で見積もる（入口の読み込み・振り分けの表・出口は含めない）。
            let mut groups: Vec<Vec<(u32, Vec<Instr>)>> = vec![];
            let mut size = 0;
            for b in blocks {
                let est = codegen::block_size(b.0, &b.1);
                match groups.last_mut() {
                    Some(g) if size + est <= FUNC_LIMIT => g.push(b),
                    _ => {
                        groups.push(vec![b]);
                        size = 0;
                    }
                }
                size += est;
            }
            for g in groups {
                let f = codegen::gen_page(&g);
                self.stats.max_func = self.stats.max_func.max(f.len() as u64);
                funcs.push(f);
                // 古い関数は最初のグループの分として捨てる
                targets.push((
                    page,
                    g.iter().map(|b| b.0).collect::<Vec<_>>(),
                    std::mem::take(&mut old),
                ));
            }
        }
        if funcs.is_empty() {
            return;
        }
        let bytes = wasm::module(&codegen::helpers(), &funcs);
        if self.loaded + bytes.len() as u64 > CODE_LIMIT {
            // 全部捨てる（作り直したページの古い関数も消える。枠も 0 に戻るので、
            // 今回のページの枠だけ下で入れ直す）。
            self.flush(code);
            self.stats.flushes += 1;
            for (_, _, old) in &mut targets {
                old.clear();
            }
        }
        let Some(host) = &mut self.host else {
            return;
        };
        match host.load(&bytes, funcs.len() as u32) {
            Ok(base) => {
                for (n, (page, idxs, old)) in targets.into_iter().enumerate() {
                    for idx in idxs {
                        code.set_slot(page, idx, COMPILED | (base + n as u32));
                    }
                    for f in old {
                        host.release(f);
                    }
                }
                self.loaded += bytes.len() as u64;
                self.stats.pages += funcs.len() as u64;
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

impl Drop for Jit {
    fn drop(&mut self) {
        // ホストのテーブルは他のマシンと共有なので、自分の関数を返す。
        if let Some(h) = &mut self.host {
            h.release_all();
        }
    }
}

/// JIT の対象の Op か（実行ループがブロックの切れ目を知るのに使う）。
#[inline(always)]
pub(crate) fn supported(i: &crate::arm::Instr) -> bool {
    codegen::supported(i)
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
