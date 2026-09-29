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
