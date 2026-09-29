//! CErulean のブラウザ版のエントリ（Web Worker・Node から使う wasm の API）。
//!
//! 段階2（計測）の最小の API: イメージ／スナップショットの読み込み、スクリプトの予定、
//! 命令数までの実行、一致確認に要る値（CPU 状態のダンプ・RAM・UART1・画面）の取り出し。
//! ハッシュ（SHA-256）は呼び出し側（Node の crypto・ブラウザの SubtleCrypto）で取る
//! （wasm に依存を足さないため。計画書 §8）。段階3 で Worker 用の API（入力・自動保存
//! など）を足す（計画書 §7.2）。
//!
//! 命令数（u64）は JS では BigInt になる。

use cerulean_core::arm::StopError;
use cerulean_core::emu::{self, RunError, Session};
use cerulean_core::jit::{self, JitHost};
use cerulean_core::script::{self, Event, Kind};
use cerulean_core::smdk2410::{INSTRUCTIONS_PER_SECOND, Machine, SDRAM_BASE};
use wasm_bindgen::prelude::*;

/// 仮想時間 1 秒あたりの命令数。
#[wasm_bindgen(js_name = instructionsPerSecond)]
pub fn instructions_per_second() -> u64 {
    INSTRUCTIONS_PER_SECOND
}

/// panic の文言をコンソールに出す（wasm の panic は trap になり、インスタンスは以後
/// 使えない。呼び出し側は RuntimeError を捕まえてインスタンスを作り直す。計画書 §6.2）。
#[wasm_bindgen(js_name = installPanicHook)]
pub fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        web_sys_console_error(&info.to_string());
    }));
}

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = console, js_name = error)]
    fn web_sys_console_error(s: &str);
}

// ---- JIT のホスト（段階5。docs/stage5-design.md §2）----
//
// 生成したモジュールは本体の線形メモリを import する。関数は本体の関数テーブルに
// 置き、Rust からテーブルの添字（wasm32 の関数ポインタ）で直接呼ぶ（段階5-2。
// JS の中継と比べて、境界の出入りが実行時間の約 2 割を占めていたため。計測は
// docs/rust-migration-plan.md の段階5）。テーブルを伸ばすため、リンク時に
// --growable-table を付ける（build.rs）。
// `new WebAssembly.Module`（同期）は Chrome のメインスレッドでは 4KB までだが、
// エミュレータは Worker（と Node）で動かす。
#[wasm_bindgen(inline_js = r#"
let mem = null, table = null, next = 0;
export function jit_init(m, t) {
  mem = m;
  table = t;
  if (next === 0) next = table.length;
}
export function jit_load(bytes, n) {
  const inst = new WebAssembly.Instance(new WebAssembly.Module(bytes), { e: { m: mem } });
  if (next + n > table.length) table.grow(next + n - table.length);
  const base = next;
  for (let i = 0; i < n; i++) table.set(base + i, inst.exports[String(i)]);
  next += n;
  return base;
}
export function jit_release(id) { table.set(id, null); }
"#)]
extern "C" {
    fn jit_init(mem: JsValue, table: JsValue);
    #[wasm_bindgen(catch)]
    fn jit_load(bytes: &[u8], n: u32) -> Result<u32, JsValue>;
    fn jit_release(id: u32);
}

/// JS の WebAssembly で生成コードを動かすホスト。テーブルの添字は使い回さない
/// （捨てた枠は null にするだけ。1 回の全消去で増えるのはページ数程度）。
struct WebJitHost {
    /// このホストが置いた関数の添字（release_all で捨てる。複数のマシンが同じ
    /// テーブルを使うので、他のホストの関数は触らない）
    live: std::collections::BTreeSet<u32>,
}

impl WebJitHost {
    fn boxed() -> Box<dyn JitHost> {
        jit_init(wasm_bindgen::memory(), wasm_bindgen::function_table());
        Box::new(WebJitHost {
            live: Default::default(),
        })
    }
}

type JitFn = extern "C" fn(u32) -> u32;

impl JitHost for WebJitHost {
    fn load(&mut self, wasm: &[u8], nfuncs: u32) -> Result<u32, String> {
        let base = jit_load(wasm, nfuncs).map_err(|e| format!("{e:?}"))?;
        self.live.extend(base..base + nfuncs);
        Ok(base)
    }
    fn call(&mut self, func: u32, ctx: u32) -> u32 {
        debug_assert!(self.live.contains(&func));
        // SAFETY: func は jit_load がこのインスタンスの関数テーブルに置いた、型
        // (i32) -> i32 の wasm 関数の添字で、まだ release していない（コアは捨てた
        // 関数の番号を呼ばない。枠を捨ててから release する）。wasm32 の関数
        // ポインタはテーブルの添字なので、この値を JitFn として呼ぶと call_indirect
        // になり、型が合わなければ trap する（メモリ安全は壊れない）。生成コードは
        // ctx が指すアドレス（コアが呼ぶ直前に &mut から作ったもの）を通して CPU の
        // 状態・RAM を書き換える（jit/mod.rs の Jit::call）。正しさは差分テストと
        // 基準シナリオで確かめる。
        let f: JitFn = unsafe { std::mem::transmute::<usize, JitFn>(func as usize) };
        f(ctx)
    }
    fn release(&mut self, func: u32) {
        if self.live.remove(&func) {
            jit_release(func);
        }
    }
    fn release_all(&mut self) {
        for f in std::mem::take(&mut self.live) {
            jit_release(f);
        }
    }
}

/// JIT とインタプリタの差分テスト（jit::selftest。Node の rust/web/tests/jit-diff.mjs が
/// 呼ぶ）。成功なら要約、食い違ったら "FAIL: 説明" を返す。
#[wasm_bindgen(js_name = jitSelfTest)]
pub fn jit_self_test(seed: u64, cases: u32, steps: u64) -> String {
    match jit::selftest::run(seed, cases, steps, &mut WebJitHost::boxed) {
        Ok(r) => format!(
            "cases {} steps {} jit-executed {} blocks {} side-exits {}",
            r.cases, r.total, r.jit_executed, r.blocks, r.side_exits
        ),
        Err(e) => format!("FAIL: {e}"),
    }
}

/// 1 台のマシンと予定したイベント列。
#[wasm_bindgen]
pub struct Emu {
    m: Machine,
    sess: Session,
    /// 最後に止まった理由（一致確認の stop の JSON。止まっていなければ空）
    stop: String,
    /// 最後に frame で作った画面の大きさ
    frame_w: u32,
    frame_h: u32,
}

impl Default for Emu {
    fn default() -> Self {
        Self::new()
    }
}

#[wasm_bindgen]
impl Emu {
    #[wasm_bindgen(constructor)]
    pub fn new() -> Emu {
        Emu {
            m: Machine::new(),
            sess: Session::new(),
            stop: String::new(),
            frame_w: 0,
            frame_h: 0,
        }
    }

    /// イメージを読み込んでリセットする。rtc はローカル時刻の年月日時分秒
    /// （フロントエンドがホストの時刻から渡す。一致確認では固定値）。
    #[wasm_bindgen(js_name = loadImage)]
    pub fn load_image(&mut self, data: &[u8], name: &str, rtc: &[i32]) -> Result<(), JsError> {
        let [y, mo, d, h, mi, s] = rtc else {
            return Err(JsError::new("rtc must have 6 elements"));
        };
        let img = cerulean_core::loader::load(data, name, 0x30000000)
            .map_err(|e| JsError::new(&e.to_string()))?;
        self.m
            .load_image(&img)
            .map_err(|e| JsError::new(&e.to_string()))?;
        self.m.set_rtc(
            *y as i64, *mo as i64, *d as i64, *h as i64, *mi as i64, *s as i64,
        );
        self.m.reset();
        Ok(())
    }

    /// スナップショットから再開する（新しい Emu に対して呼ぶ）。戻り値はイメージ ID。
    #[wasm_bindgen(js_name = loadSnapshot)]
    pub fn load_snapshot(&mut self, data: &[u8]) -> Result<String, JsError> {
        self.m
            .load_snapshot(data)
            .map_err(|e| JsError::new(&e.to_string()))
    }

    /// 全状態を保存する（無圧縮。命令境界で呼ぶ）。
    #[wasm_bindgen(js_name = saveSnapshot)]
    pub fn save_snapshot(&mut self, image_id: &str) -> Result<Vec<u8>, JsError> {
        let mut buf = Vec::with_capacity(135 << 20);
        self.m
            .save_snapshot(&mut buf, image_id)
            .map_err(|e| JsError::new(&e.to_string()))?;
        Ok(buf)
    }

    /// 入力スクリプトを予定に加える（今の命令数より前のイベントは読み飛ばす）。
    /// shot/snap は無視する（ファイル出力は呼び出し側の責務）。
    #[wasm_bindgen(js_name = scheduleScript)]
    pub fn schedule_script(&mut self, text: &str) -> Result<(), JsError> {
        let evs = script::parse(text, INSTRUCTIONS_PER_SECOND)
            .map_err(|e| JsError::new(&e.to_string()))?;
        for ev in &evs {
            emu::validate(&self.m, ev)
                .map_err(|e| JsError::new(&format!("script line {}: {e}", ev.line)))?;
        }
        let start = self.m.steps();
        self.sess
            .schedule(evs.into_iter().filter(|e| e.step >= start));
        Ok(())
    }

    /// 予定イベントを命令境界で適用しながら、命令数 until まで進める（until ちょうどの
    /// イベントは次の run で適用する）。戻り値は "ok"（until に達した）・"quit"
    /// （スクリプトの quit）・"stop"（エラーで停止。理由は stopJson）。
    pub fn run(&mut self, until: u64) -> String {
        let mut apply = |m: &mut Machine, ev: &Event| -> Result<bool, String> {
            match ev.kind {
                Kind::Quit => Ok(true),
                Kind::Shot | Kind::Snap => Ok(false),
                _ => emu::apply_input(m, ev),
            }
        };
        match self.sess.run(&mut self.m, until, &mut apply) {
            Ok(false) => "ok".into(),
            Ok(true) => {
                self.stop = r#"{"kind":"quit"}"#.into();
                "quit".into()
            }
            Err(RunError::Event { .. }) => {
                self.stop = r#"{"kind":"event-error"}"#.into();
                "stop".into()
            }
            Err(RunError::Stop(StopError::Undefined(u))) => {
                self.stop = format!(r#"{{"kind":"undefined","pc":{},"word":{}}}"#, u.pc, u.word);
                "stop".into()
            }
            Err(RunError::Stop(StopError::Bus(b))) => {
                self.stop = format!(
                    r#"{{"kind":"bus-error","addr":{},"write":{}}}"#,
                    b.addr, b.write
                );
                "stop".into()
            }
        }
    }

    /// 最後に止まった理由（testdata/golden/README.md の stop の JSON）。
    #[wasm_bindgen(js_name = stopJson)]
    pub fn stop_json(&self) -> String {
        self.stop.clone()
    }

    /// リセットからの命令数。
    pub fn steps(&self) -> u64 {
        self.m.steps()
    }

    /// アイドルスキップの有無（既定は有効。結果は同じ。計測・診断用）。
    #[wasm_bindgen(js_name = setIdleSkip)]
    pub fn set_idle_skip(&mut self, on: bool) {
        self.m.set_idle_skip(on);
    }

    /// JIT の有無（既定は無効）。threshold はブロックの先頭として何回入ったら
    /// コンパイルするか、batch は何ブロックを 1 モジュールにまとめるか。
    /// どの値でもゲストの状態は同じ（段階5）。
    #[wasm_bindgen(js_name = setJit)]
    pub fn set_jit(&mut self, on: bool, threshold: u32, batch: u32) {
        let host = on.then(WebJitHost::boxed);
        self.m.set_jit(host, threshold, batch);
    }

    /// JIT の計測値（JSON）。無効にした理由があれば error に入る。
    #[wasm_bindgen(js_name = jitStats)]
    pub fn jit_stats(&self) -> String {
        let j = self.m.jit();
        let s = j.stats();
        let err = j
            .error()
            .map(|e| format!(r#","error":{:?}"#, e))
            .unwrap_or_default();
        format!(
            r#"{{"blocks":{},"pages":{},"modules":{},"bytes":{},"calls":{},"executed":{},"side_exits":{},"exit_page":{},"exit_thumb":{},"exit_other":{},"flushes":{}{err}}}"#,
            s.blocks,
            s.pages,
            s.modules,
            s.bytes,
            s.calls,
            s.executed,
            s.side_exits,
            s.exit_page,
            s.exit_thumb,
            s.exit_other,
            s.flushes
        )
    }

    /// アイドルスキップで飛ばした命令数の累計。
    #[wasm_bindgen(js_name = idleSkipped)]
    pub fn idle_skipped(&self) -> u64 {
        self.m.idle_skipped()
    }

    /// デコード済みのページ数（メモリ予算の計測用。1 ページ約 16KB）。
    #[wasm_bindgen(js_name = codePages)]
    pub fn code_pages(&self) -> usize {
        self.m.sys.code.page_count()
    }

    /// UART1 が送信したバイトを取り出す。
    #[wasm_bindgen(js_name = takeUart)]
    pub fn take_uart(&mut self) -> Vec<u8> {
        self.m.take_uart1()
    }

    /// CPU 状態のダンプ（testdata/golden/README.md、212 バイト）。
    #[wasm_bindgen(js_name = cpuDump)]
    pub fn cpu_dump(&self) -> Vec<u8> {
        self.m.cpu_dump()
    }

    /// SDRAM 全体（128MB）の写し（ハッシュを取る用）。
    pub fn ram(&self) -> Vec<u8> {
        self.m
            .sys
            .bus
            .ram(SDRAM_BASE)
            .map(|(r, _)| r.to_vec())
            .unwrap_or_default()
    }

    /// 画面（RGBA、行の詰め物なし）。表示が無効なら空。幅・高さは直後に
    /// frameWidth/frameHeight で読む。
    pub fn frame(&mut self) -> Vec<u8> {
        match self.m.frame() {
            Ok((f, _)) => {
                (self.frame_w, self.frame_h) = (f.width, f.height);
                f.rgba
            }
            Err(_) => {
                (self.frame_w, self.frame_h) = (0, 0);
                vec![]
            }
        }
    }

    #[wasm_bindgen(js_name = frameWidth)]
    pub fn frame_width(&self) -> u32 {
        self.frame_w
    }

    #[wasm_bindgen(js_name = frameHeight)]
    pub fn frame_height(&self) -> u32 {
        self.frame_h
    }
}
