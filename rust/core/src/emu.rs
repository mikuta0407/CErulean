//! フロントエンド共通の実行制御（Go の emu パッケージ）: 命令数つきの入力
//! イベントを命令境界で適用しながらマシンを進め、対話入力を記録する。
//!
//! CLI（スクリプト再生）・ブラウザ版から同じ規則で使うためのもので、OS に
//! 依存しない（ファイル IO・壁時計は呼び出し側の責務）。決定論性の要は次の 1 点だけ:
//!
//! > イベントは「steps() がその命令数に達した時点（次の命令の実行前）」に適用する。
//!
//! 対話入力（inject）は適用した時点の steps() を刻んで記録するので、記録を
//! スクリプトとして再生すると、同じ命令境界で同じ入力が入り、同じ状態になる。
//!
//! TODO(将来の PXA27x): 今は smdk2410 の Machine を直接使う。マシンを差し替える
//! ときに、ここが使う操作（run_until・steps・入力 API）を trait にする。

use std::fmt;

use crate::arm::StopError;
use crate::script::{Event, Kind};
use crate::smdk2410::{KEY_SCAN_CODES, Machine};

/// 予定・記録したイベント列と、次に適用する位置。
#[derive(Clone, Debug, Default)]
pub struct Session {
    /// 予定（step 昇順）。先頭 next 件は適用済み
    events: Vec<Event>,
    next: usize,
    recording: bool,
    record: Vec<Event>,
    rec_start: u64,
}

/// run のエラー。
#[derive(Debug)]
pub enum RunError<E> {
    /// 予定したイベントの適用に失敗した
    Event { event: Event, err: E },
    /// エミュレーションが止まった（未実装命令・バスエラー）
    Stop(StopError),
}

impl<E: fmt::Display> fmt::Display for RunError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RunError::Event { event, err } => write!(f, "script line {}: {err}", event.line),
            RunError::Stop(s) => s.fmt(f),
        }
    }
}

impl Session {
    pub fn new() -> Session {
        Session::default()
    }

    /// イベントを予定に加える（step 順。同じ命令数なら加えた順）。
    /// 現在の steps() より前のイベントは、次の run の最初に適用される。
    pub fn schedule(&mut self, evs: impl IntoIterator<Item = Event>) {
        for ev in evs {
            let mut i = self.events.len();
            while i > self.next && self.events[i - 1].step > ev.step {
                i -= 1;
            }
            self.events.insert(i, ev);
        }
    }

    /// 未適用の予定イベント数。
    pub fn pending(&self) -> usize {
        self.events.len() - self.next
    }

    /// 次の予定イベントの命令数。
    pub fn next_event_step(&self) -> Option<u64> {
        self.events.get(self.next).map(|e| e.step)
    }

    /// 予定イベントを命令境界で適用しながら、steps() が until に達するまで
    /// 進める。until ちょうどに予定されたイベントは適用せずに戻る（次の run の
    /// 最初に適用される）。apply が quit=true を返したら Ok(true) で直ちに戻る。
    /// apply はイベントを 1 個適用する（入力は apply_input に委ね、shot/snap/quit
    /// のようなファイル出力や終了は呼び出し側が扱う）。
    pub fn run<E>(
        &mut self,
        m: &mut Machine,
        until: u64,
        apply: &mut impl FnMut(&mut Machine, &Event) -> Result<bool, E>,
    ) -> Result<bool, RunError<E>> {
        loop {
            let steps = m.steps();
            while self.next < self.events.len() && self.events[self.next].step <= steps {
                let ev = self.events[self.next].clone();
                self.next += 1;
                match apply(m, &ev) {
                    Ok(true) => return Ok(true),
                    Ok(false) => {}
                    Err(err) => return Err(RunError::Event { event: ev, err }),
                }
            }
            if steps >= until {
                return Ok(false);
            }
            let target = self.next_event_step().map_or(until, |n| n.min(until));
            m.run_until(target).map_err(RunError::Stop)?;
            if m.steps() >= until {
                return Ok(false); // until ちょうどのイベントは次の run で適用する
            }
        }
    }

    /// 対話入力を今の命令境界（steps()）で直ちに適用し、記録中なら記録する。
    /// ev.step は上書きされる。入力（down/move/up/key）以外は不可。
    pub fn inject(&mut self, m: &mut Machine, mut ev: Event) -> Result<(), String> {
        if !is_input(ev.kind) {
            return Err(format!("emu: {} cannot be injected", ev.kind));
        }
        validate(m, &ev)?;
        ev.step = m.steps();
        ev.line = 0;
        apply_input(m, &ev)?;
        if self.recording {
            self.record.push(ev);
        }
        Ok(())
    }

    /// 記録を始める（それまでの記録は捨てる）。再生の起点になる状態
    /// （スナップショット）は呼び出し側が同じ命令境界で保存すること。
    pub fn start_recording(&mut self, m: &Machine) {
        self.recording = true;
        self.record.clear();
        self.rec_start = m.steps();
    }

    pub fn recording(&self) -> bool {
        self.recording
    }

    /// 記録を止め、記録開始の命令数と記録したイベントを返す。
    pub fn stop_recording(&mut self) -> (u64, Vec<Event>) {
        self.recording = false;
        (self.rec_start, std::mem::take(&mut self.record))
    }
}

fn is_input(k: Kind) -> bool {
    matches!(
        k,
        Kind::TouchDown | Kind::TouchMove | Kind::TouchUp | Kind::KeyDown | Kind::KeyUp
    )
}

/// 入力イベントをマシンに適用する。shot/snap/quit はファイル出力や終了を
/// 伴うので呼び出し側が扱う（ここではエラー）。
pub fn apply_input(m: &mut Machine, ev: &Event) -> Result<bool, String> {
    match ev.kind {
        // ペンが上がっていれば move も down と同じ（machine の規約）。
        Kind::TouchDown | Kind::TouchMove => m.touch_down(ev.x, ev.y).map_err(|e| e.to_string())?,
        Kind::TouchUp => m.touch_up(),
        Kind::KeyDown => m.key_down(&ev.key).map_err(|e| e.to_string())?,
        Kind::KeyUp => m.key_up(&ev.key).map_err(|e| e.to_string())?,
        k => return Err(format!("{k}: not handled")),
    }
    Ok(false)
}

/// 実行前にマシン依存の妥当性（座標範囲・キー名）を検査する
/// （長い実行の途中でスクリプトの誤りに気づくのを避けるため）。
pub fn validate(m: &Machine, ev: &Event) -> Result<(), String> {
    match ev.kind {
        Kind::TouchDown | Kind::TouchMove => {
            let (w, h) = m.touch_screen_size();
            if ev.x < 0 || ev.y < 0 || ev.x >= w as i64 || ev.y >= h as i64 {
                return Err(format!(
                    "{}: ({},{}) outside the {w}x{h} screen",
                    ev.kind, ev.x, ev.y
                ));
            }
        }
        Kind::KeyDown | Kind::KeyUp if !KEY_SCAN_CODES.iter().any(|(n, _)| *n == ev.key) => {
            let names: Vec<&str> = KEY_SCAN_CODES.iter().map(|(n, _)| *n).collect();
            return Err(format!(
                "unknown key {:?} (available: {})",
                ev.key,
                names.join(" ")
            ));
        }
        _ => {}
    }
    Ok(())
}

/// 記録（stop_recording の結果）を、再生用のスクリプトとして書く。
/// 先頭のコメントに再生の起点（記録開始時点のスナップショット）を残す。
/// 時刻は絶対命令数なので、同じスナップショットから再生すれば同じ命令境界で
/// 同じ入力が入る。
pub fn format_recording(
    start_snap: &str,
    image_id: &str,
    start: u64,
    events: &[Event],
) -> Result<String, String> {
    let mut header = vec![
        "CErulean input recording".to_string(),
        format!("start step: {start}"),
        format!("start snapshot: {start_snap}"),
    ];
    if !image_id.is_empty() {
        header.push(format!("image sha256: {image_id}"));
    }
    header.push("replay: cerulean run --snap-load <start snapshot> --script <this file>".into());
    crate::script::format(&header, events).map_err(|e| e.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loader::{Format, Image, Segment};

    /// 「カウンタを増やし続けるだけ」のプログラムを載せたマシン。
    fn spin_machine() -> Machine {
        let prog = vec![
            0x01, 0x00, 0x80, 0xE2, // ADD r0, r0, #1
            0xFD, 0xFF, 0xFF, 0xEA, // B -4 (ADD へ)
        ];
        let img = Image {
            format: Format::Bin,
            start: 0x80000000,
            length: 8,
            entry: 0x80000000,
            segs: vec![Segment {
                addr: 0x80000000,
                data: prog,
            }],
            records: vec![],
        };
        let mut m = Machine::new();
        m.load_image(&img).unwrap();
        m.reset();
        m
    }

    fn key(step: u64, kind: Kind, k: &str) -> Event {
        Event {
            key: k.into(),
            ..Event::new(step, kind)
        }
    }

    /// run(until) は until ちょうどのイベントを適用せずに戻り、次の run の
    /// 最初に適用する。予定は命令数順・同時刻は加えた順。
    #[test]
    fn run_event_boundaries() {
        let mut m = spin_machine();
        let mut s = Session::new();
        let mut applied: Vec<(String, u64)> = vec![];
        let mut apply = |m: &mut Machine, ev: &Event| -> Result<bool, String> {
            applied.push((ev.key.clone(), m.steps()));
            Ok(ev.kind == Kind::Quit)
        };
        s.schedule([
            key(100, Kind::KeyDown, "b"),
            key(50, Kind::KeyDown, "a"),
            key(100, Kind::KeyDown, "c"),
            key(200, Kind::Quit, "quit"),
            key(300, Kind::KeyDown, "never"),
        ]);
        assert!(!s.run(&mut m, 100, &mut apply).unwrap());
        assert_eq!(m.steps(), 100);
        assert!(s.run(&mut m, 1000, &mut apply).unwrap(), "quit");
        assert_eq!(m.steps(), 200);
        let want = [("a", 50), ("b", 100), ("c", 100), ("quit", 200)];
        assert_eq!(
            applied
                .iter()
                .map(|(k, s)| (k.as_str(), *s))
                .collect::<Vec<_>>(),
            want
        );
        assert_eq!(s.pending(), 1);
    }

    /// inject は今の命令境界で適用し、記録に命令数を刻む。
    #[test]
    fn inject_records_steps() {
        let mut m = spin_machine();
        let mut s = Session::new();
        m.run_until(10).unwrap();
        s.start_recording(&m);
        m.run_until(25).unwrap();
        s.inject(&mut m, key(0, Kind::KeyDown, "Enter")).unwrap();
        s.inject(
            &mut m,
            Event {
                x: 5,
                y: 6,
                ..Event::new(0, Kind::TouchDown)
            },
        )
        .unwrap();
        assert!(s.inject(&mut m, Event::new(0, Kind::Quit)).is_err());
        assert!(s.inject(&mut m, key(0, Kind::KeyDown, "Nope")).is_err());
        let (start, evs) = s.stop_recording();
        assert_eq!(start, 10);
        assert_eq!(evs.iter().map(|e| e.step).collect::<Vec<_>>(), [25, 25]);
        let text = format_recording("a.snap", "abc", start, &evs).unwrap();
        assert!(
            text.contains("@25i key down Enter\n@25i down 5 6\n"),
            "{text}"
        );
    }
}
