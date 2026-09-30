//! CErulean のブラウザ版のエントリ（Web Worker・Node から使う wasm の API）。
//!
//! 段階2（計測）の API: イメージ／スナップショットの読み込み、スクリプトの予定、
//! 命令数までの実行、一致確認に要る値（CPU 状態のダンプ・RAM・UART1・画面）の取り出し。
//! 段階3 で対話入力（input）と記録（recordStart/recordStop）を足した。
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
            "cases {} steps {} jit-executed {} blocks {} side-exits {} links {}",
            r.cases, r.total, r.jit_executed, r.blocks, r.side_exits, r.links
        ),
        Err(e) => format!("FAIL: {e}"),
    }
}

// ---- スナップショットの小分けの読み書き（段階3）----
//
// 128MB の RAM を含むスナップショットを wasm の中で 1 本の Vec に組み立てると、
// 線形メモリがその分伸びたまま縮まない（§6.2）。JS の関数に 1MB ずつ渡す／受け取る。
#[wasm_bindgen(inline_js = r#"
export function snap_sink(f, b) { f(b.slice()); }
export function snap_src(f, buf) { return f(buf); }
"#)]
extern "C" {
    #[wasm_bindgen(catch)]
    fn snap_sink(f: &JsValue, b: &[u8]) -> Result<(), JsValue>;
    #[wasm_bindgen(catch)]
    fn snap_src(f: &JsValue, buf: &mut [u8]) -> Result<u32, JsValue>;
}

const SNAP_PIECE: usize = 1 << 20;

/// JS の関数 f(Uint8Array) に書く（1MB ずつ。渡した配列は f のもの）。
struct JsSink<'a> {
    f: &'a JsValue,
    buf: Vec<u8>,
}

impl JsSink<'_> {
    fn flush_buf(&mut self) -> std::io::Result<()> {
        if !self.buf.is_empty() {
            snap_sink(self.f, &self.buf).map_err(|e| std::io::Error::other(format!("{e:?}")))?;
            self.buf.clear();
        }
        Ok(())
    }
}

impl std::io::Write for JsSink<'_> {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        if self.buf.len() + data.len() > SNAP_PIECE {
            self.flush_buf()?;
        }
        if data.len() >= SNAP_PIECE {
            // 大きな書き込み（RAM）は溜めずに 1MB ずつ渡す
            for c in data.chunks(SNAP_PIECE) {
                snap_sink(self.f, c).map_err(|e| std::io::Error::other(format!("{e:?}")))?;
            }
        } else {
            self.buf.extend_from_slice(data);
        }
        Ok(data.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.flush_buf()
    }
}

/// JS の関数 f(Uint8Array) -> 読んだバイト数 から読む（0 で終わり）。
struct JsSource<'a> {
    f: &'a JsValue,
}

impl std::io::Read for JsSource<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = buf.len().min(SNAP_PIECE);
        let got =
            snap_src(self.f, &mut buf[..n]).map_err(|e| std::io::Error::other(format!("{e:?}")))?;
        Ok((got as usize).min(n))
    }
}

/// 1 台のマシンと予定したイベント列。
#[wasm_bindgen]
pub struct Emu {
    m: Machine,
    sess: Session,
    /// ネットワーク（中継サーバー経由）がオンの間のスタック（cerulean-net）
    net: Option<cerulean_net::Stack>,
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
            net: None,
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

    /// 全状態を JS の関数 sink(Uint8Array) に 1MB ずつ書く（命令境界で呼ぶ）。
    /// UART1 の送信バイトは保存されないので、先に takeUart で取り出しておくこと。
    #[wasm_bindgen(js_name = saveSnapshotTo)]
    pub fn save_snapshot_to(&mut self, image_id: &str, sink: JsValue) -> Result<(), JsError> {
        let mut w = JsSink {
            f: &sink,
            buf: Vec::with_capacity(SNAP_PIECE),
        };
        self.m
            .save_snapshot(&mut w, image_id)
            .map_err(|e| JsError::new(&e.to_string()))?;
        std::io::Write::flush(&mut w).map_err(|e| JsError::new(&e.to_string()))
    }

    /// JS の関数 src(Uint8Array) -> 読んだバイト数 から読んで再開する（新しい Emu に
    /// 対して呼ぶ。失敗したらその Emu は捨てる）。戻り値はイメージ ID。
    #[wasm_bindgen(js_name = loadSnapshotFrom)]
    pub fn load_snapshot_from(&mut self, src: JsValue) -> Result<String, JsError> {
        self.m
            .load_snapshot(std::io::BufReader::with_capacity(
                SNAP_PIECE,
                JsSource { f: &src },
            ))
            .map_err(|e| JsError::new(&e.to_string()))
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
                // TODO(段階3 のカード): ブラウザ版のカードの挿抜はメニューから行う（未実装）
                Kind::CardInsert | Kind::CardEject => {
                    Err("card: not supported in scripts here".into())
                }
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

    /// 対話入力を今の命令境界で適用する（run の合間に呼ぶ。記録中なら今の命令数で
    /// 記録する）。t は "down"・"move"（x, y は 240×320 の画面座標）・"up"・
    /// "keydown"・"keyup"（key はキー名。smdk2410 の KEY_SCAN_CODES）。
    pub fn input(&mut self, t: &str, x: i32, y: i32, key: &str) -> Result<(), JsError> {
        let kind = match t {
            "down" => Kind::TouchDown,
            "move" => Kind::TouchMove,
            "up" => Kind::TouchUp,
            "keydown" => Kind::KeyDown,
            "keyup" => Kind::KeyUp,
            _ => return Err(JsError::new(&format!("unknown input {t:?}"))),
        };
        let ev = Event {
            x: x as i64,
            y: y as i64,
            key: key.into(),
            ..Event::new(0, kind)
        };
        self.sess
            .inject(&mut self.m, ev)
            .map_err(|e| JsError::new(&e))
    }

    /// ストレージカードを挿す（今の命令境界。記録中なら `card insert <name>` として
    /// 記録する。name は再生のときにイメージを置くファイル名）。
    #[wasm_bindgen(js_name = cardInsert)]
    pub fn card_insert(&mut self, disk: Vec<u8>, name: &str) -> Result<(), JsError> {
        self.m
            .insert_card(disk)
            .map_err(|e| JsError::new(&e.to_string()))?;
        let ev = Event {
            path: name.into(),
            ..Event::new(0, Kind::CardInsert)
        };
        self.sess.record_applied(&self.m, ev);
        Ok(())
    }

    /// カードを抜いてディスクイメージ（ゲストの書き込みを含む）を返す。挿していなければ
    /// undefined。記録中なら `card eject` として記録する。
    #[wasm_bindgen(js_name = cardEject)]
    pub fn card_eject(&mut self) -> Option<Vec<u8>> {
        let disk = self.m.eject_card()?;
        self.sess
            .record_applied(&self.m, Event::new(0, Kind::CardEject));
        Some(disk)
    }

    #[wasm_bindgen(js_name = cardInserted)]
    pub fn card_inserted(&self) -> bool {
        self.m.card_disk().is_some()
    }

    /// 挿しているカードの今の中身の写し（書き出し用。状態は変えない）。
    #[wasm_bindgen(js_name = cardDisk)]
    pub fn card_disk(&self) -> Option<Vec<u8>> {
        self.m.card_disk().map(|d| d.to_vec())
    }

    // ---- ネットワーク（イーサネットカードと cerulean-net のスタック）----
    //
    // 中継サーバーとのやり取り（WebSocket）は Worker の JS が行う。スタックが作った
    // フレームは、作った時点の命令境界で `net rx` の入力としてゲストに渡し、記録する
    // （記録を再生すればネットワークなしで同じ状態になる）。

    /// イーサネットカードを挿す（今の命令境界。記録中なら `nic insert` として記録する）。
    #[wasm_bindgen(js_name = nicInsert)]
    pub fn nic_insert(&mut self) -> Result<(), JsError> {
        let ev = Event {
            data: cerulean_core::pccard::ne2000::DEFAULT_MAC.to_vec(),
            ..Event::new(0, Kind::NicInsert)
        };
        self.sess
            .inject(&mut self.m, ev)
            .map_err(|e| JsError::new(&e))
    }

    /// イーサネットカードを抜く（挿していなければ何もしない）。
    #[wasm_bindgen(js_name = nicEject)]
    pub fn nic_eject(&mut self) -> Result<(), JsError> {
        if self.m.nic_mac().is_none() {
            return Ok(());
        }
        self.sess
            .inject(&mut self.m, Event::new(0, Kind::NicEject))
            .map_err(|e| JsError::new(&e))
    }

    #[wasm_bindgen(js_name = nicInserted)]
    pub fn nic_inserted(&self) -> bool {
        self.m.nic_mac().is_some()
    }

    /// スタックを作る・捨てる（捨てると外への接続はすべて忘れる）。
    #[wasm_bindgen(js_name = netEnable)]
    pub fn net_enable(&mut self, on: bool) {
        if on {
            self.net.get_or_insert_with(cerulean_net::Stack::new);
        } else {
            self.net = None;
        }
    }

    /// ゲストが送ったフレームをスタックに渡し、時間を進め（再送）、ゲストへのフレームを
    /// 渡す。戻り値は中継への依頼の列（[種類 u8][番号 u32][長さ u32][中身] の繰り返し。
    /// 種類 1 = 接続（中身はポート u16 と接続先の名前）、2 = 送信、3 = 送信の終わり、
    /// 4 = 切断。整数はリトルエンディアン）。now_ms は仮想時間のミリ秒。
    #[wasm_bindgen(js_name = netStep)]
    pub fn net_step(&mut self, now_ms: f64) -> Result<Vec<u8>, JsError> {
        let sent = self.m.net_take_tx();
        let Some(net) = &mut self.net else {
            return Ok(Vec::new());
        };
        let now = now_ms.max(0.0) as u64;
        for f in &sent {
            net.input(now, f);
        }
        net.poll(now);
        self.deliver()?;
        let mut out = Vec::new();
        let Some(net) = &mut self.net else {
            return Ok(out);
        };
        for r in net.take_requests() {
            let (kind, id, body) = match r {
                cerulean_net::Request::Connect { id, target, port } => {
                    let host = match target {
                        cerulean_net::Target::Ip(ip) => {
                            format!("{}.{}.{}.{}", ip[0], ip[1], ip[2], ip[3])
                        }
                        cerulean_net::Target::Host(h) => h,
                    };
                    let mut b = port.to_le_bytes().to_vec();
                    b.extend_from_slice(host.as_bytes());
                    (1u8, id, b)
                }
                cerulean_net::Request::Send { id, data } => (2, id, data),
                cerulean_net::Request::Shutdown { id } => (3, id, Vec::new()),
                cerulean_net::Request::Close { id } => (4, id, Vec::new()),
            };
            out.push(kind);
            out.extend_from_slice(&id.to_le_bytes());
            out.extend_from_slice(&(body.len() as u32).to_le_bytes());
            out.extend_from_slice(&body);
        }
        Ok(out)
    }

    /// スタックが作ったフレームを今の命令境界でゲストに渡す（記録する）。
    fn deliver(&mut self) -> Result<(), JsError> {
        let Some(net) = &mut self.net else {
            return Ok(());
        };
        for f in net.take_frames() {
            let ev = Event {
                data: f,
                ..Event::new(0, Kind::NetRx)
            };
            self.sess
                .inject(&mut self.m, ev)
                .map_err(|e| JsError::new(&e))?;
        }
        Ok(())
    }

    /// 外への接続の結果。
    #[wasm_bindgen(js_name = netConnected)]
    pub fn net_connected(&mut self, id: u32, ok: bool) -> Result<(), JsError> {
        if let Some(n) = &mut self.net {
            n.connected(id, ok);
        }
        self.deliver()
    }

    /// その接続にあと何バイト netRecv してよいか。
    #[wasm_bindgen(js_name = netTxRoom)]
    pub fn net_tx_room(&self, id: u32) -> u32 {
        self.net.as_ref().map_or(0, |n| n.tx_room(id) as u32)
    }

    /// 外から届いたバイト列（netTxRoom 以下にすること）。
    #[wasm_bindgen(js_name = netRecv)]
    pub fn net_recv(&mut self, id: u32, data: &[u8]) -> Result<(), JsError> {
        if let Some(n) = &mut self.net {
            n.recv(id, data);
        }
        self.deliver()
    }

    /// 外の接続が送り終えた（EOF）。
    #[wasm_bindgen(js_name = netRemoteClosed)]
    pub fn net_remote_closed(&mut self, id: u32) -> Result<(), JsError> {
        if let Some(n) = &mut self.net {
            n.remote_closed(id);
        }
        self.deliver()
    }

    /// 外の接続が失敗・切断した。
    #[wasm_bindgen(js_name = netRemoteReset)]
    pub fn net_remote_reset(&mut self, id: u32) -> Result<(), JsError> {
        if let Some(n) = &mut self.net {
            n.remote_reset(id);
        }
        self.deliver()
    }

    /// 生きている接続の数（表示用）。
    #[wasm_bindgen(js_name = netConnections)]
    pub fn net_connections(&self) -> u32 {
        self.net.as_ref().map_or(0, |n| n.connections() as u32)
    }

    /// ゲストの時計（RTC）を今の命令境界で合わせる（記録中なら記録する）。rtc は
    /// ローカル時刻の年月日時分秒（再開時にフロントエンドがホストの時刻から渡す）。
    #[wasm_bindgen(js_name = setClock)]
    pub fn set_clock(&mut self, rtc: &[i32]) -> Result<(), JsError> {
        let [y, mo, d, h, mi, s] = rtc else {
            return Err(JsError::new("rtc must have 6 elements"));
        };
        let ev = Event {
            rtc: [
                *y as i64, *mo as i64, *d as i64, *h as i64, *mi as i64, *s as i64,
            ],
            ..Event::new(0, Kind::Rtc)
        };
        self.sess
            .inject(&mut self.m, ev)
            .map_err(|e| JsError::new(&e))
    }

    /// 入力の記録を始める（再生の起点のスナップショットは呼び出し側が同じ命令境界で
    /// 保存する）。
    #[wasm_bindgen(js_name = recordStart)]
    pub fn record_start(&mut self) {
        self.sess.start_recording(&self.m);
    }

    /// 記録中か。
    pub fn recording(&self) -> bool {
        self.sess.recording()
    }

    /// 記録を止め、再生用のスクリプト（絶対命令数）を返す。start_snap は起点の
    /// スナップショットの名前（コメントに残す）。
    #[wasm_bindgen(js_name = recordStop)]
    pub fn record_stop(&mut self, start_snap: &str, image_id: &str) -> Result<String, JsError> {
        let (start, evs) = self.sess.stop_recording();
        emu::format_recording(start_snap, image_id, start, &evs).map_err(|e| JsError::new(&e))
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
            r#"{{"blocks":{},"pages":{},"modules":{},"bytes":{},"max_func":{},"calls":{},"executed":{},"side_exits":{},"exit_page":{},"exit_thumb":{},"exit_other":{},"links":{},"flushes":{}{err}}}"#,
            s.blocks,
            s.pages,
            s.modules,
            s.bytes,
            s.max_func,
            s.calls,
            s.executed,
            s.side_exits,
            s.exit_page,
            s.exit_thumb,
            s.exit_other,
            s.links,
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

    /// SDRAM（128MB）の線形メモリ上の位置。JS は wasm の memory の上の view として
    /// 読む（ram() と違い複製を作らない。wasm の線形メモリを伸ばさないため。§6.2）。
    /// view は次に wasm を呼ぶまでの間だけ使うこと（メモリが伸びると無効になる）。
    #[wasm_bindgen(js_name = ramPtr)]
    pub fn ram_ptr(&self) -> u32 {
        self.m
            .sys
            .bus
            .ram(SDRAM_BASE)
            .map(|(r, _)| r.as_ptr() as usize as u32)
            .unwrap_or(0)
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

// ---- ストレージカードのイメージ（抜いている間の出し入れ。cerulean-fat）----

/// 抜いているカードのディスクイメージ（MBR＋FAT）。ファイルの出し入れをして、
/// bytes() で取り出したものを Emu.cardInsert に渡す。
#[wasm_bindgen]
pub struct CardImage {
    d: Vec<u8>,
}

fn fat_err(e: cerulean_fat::Error) -> JsError {
    JsError::new(&e.to_string())
}

fn fat_time(t: &[i32]) -> Result<cerulean_fat::Timestamp, JsError> {
    let [y, mo, d, h, mi, s] = t else {
        return Err(JsError::new("time must have 6 elements"));
    };
    Ok(cerulean_fat::Timestamp {
        year: (*y).clamp(0, u16::MAX as i32) as u16,
        month: *mo as u8,
        day: *d as u8,
        hour: *h as u8,
        minute: *mi as u8,
        second: *s as u8,
    })
}

/// JSON の文字列（制御文字と「"」「\」をエスケープする）。
fn json_str(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    o.push('"');
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

#[wasm_bindgen]
impl CardImage {
    /// 既存のイメージを開く（FAT として開けなければエラー）。
    #[wasm_bindgen(constructor)]
    pub fn new(bytes: Vec<u8>) -> Result<CardImage, JsError> {
        let mut d = bytes;
        cerulean_fat::Fs::open(&mut d).map_err(fat_err)?;
        Ok(CardImage { d })
    }

    /// 空のカード（FAT16。大きさは MB 単位）。
    pub fn format(mb: u32, label: &str) -> Result<CardImage, JsError> {
        let d = cerulean_fat::format(mb as u64 * (1 << 20), label).map_err(fat_err)?;
        Ok(CardImage { d })
    }

    fn fs(&mut self) -> Result<cerulean_fat::Fs<'_>, JsError> {
        cerulean_fat::Fs::open(&mut self.d).map_err(fat_err)
    }

    /// フォルダの一覧（JSON の配列: name・dir・size・modified [年,月,日,時,分,秒]）。
    pub fn list(&mut self, path: &str) -> Result<String, JsError> {
        let fs = self.fs()?;
        let v = fs.list(path).map_err(fat_err)?;
        let items: Vec<String> = v
            .iter()
            .map(|e| {
                let t = e.modified;
                format!(
                    r#"{{"name":{},"dir":{},"size":{},"modified":[{},{},{},{},{},{}]}}"#,
                    json_str(&e.name),
                    e.is_dir,
                    e.size,
                    t.year,
                    t.month,
                    t.day,
                    t.hour,
                    t.minute,
                    t.second
                )
            })
            .collect();
        Ok(format!("[{}]", items.join(",")))
    }

    pub fn read(&mut self, path: &str) -> Result<Vec<u8>, JsError> {
        self.fs()?.read_file(path).map_err(fat_err)
    }

    /// ファイルを書く（あれば置き換え、親のフォルダは作る）。t はローカル時刻。
    pub fn write(&mut self, path: &str, data: &[u8], t: &[i32]) -> Result<(), JsError> {
        let t = fat_time(t)?;
        self.fs()?.write_file(path, data, t).map_err(fat_err)
    }

    pub fn mkdir(&mut self, path: &str, t: &[i32]) -> Result<(), JsError> {
        let t = fat_time(t)?;
        self.fs()?.mkdir(path, t).map_err(fat_err)
    }

    /// ファイルかフォルダ（中身ごと）を消す。
    pub fn remove(&mut self, path: &str) -> Result<(), JsError> {
        self.fs()?.remove(path, true).map_err(fat_err)
    }

    #[wasm_bindgen(js_name = freeBytes)]
    pub fn free_bytes(&mut self) -> Result<f64, JsError> {
        Ok(self.fs()?.free_bytes() as f64)
    }

    /// イメージの写し。
    pub fn bytes(&self) -> Vec<u8> {
        self.d.clone()
    }
}
