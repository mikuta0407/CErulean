//! 決定論的な入力スクリプト（タップ・キー操作・画面保存を仮想時刻つきで並べた
//! テキスト）の解釈と書き出し（Go の script パッケージ）。OS に依存しない
//! （文字列から読むだけ）ので、CLI 以外（ブラウザ版の記録の再生等）からも使える。
//!
//! 書式（ユーザー確認済み 2026-09。Go 版との互換は要件ではないが、基準シナリオの
//! スクリプトをそのまま使うため当面は同じにする）: 1 行 1 コマンド、"#" 以降は
//! コメント。
//!
//! ```text
//! <時刻> <コマンド> [引数...]
//! ```
//!
//! 時刻:
//!
//! ```text
//! @<量>  絶対時刻（リセットからの仮想時間）
//! +<量>  直前のコマンドの終了時刻からの相対（tap/press は押下時間の後が終了）
//! 量の単位: s（秒）・ms（ミリ秒）・i（命令数）。s/ms は小数可（例 1.5s）。
//! ```
//!
//! 仮想時間は命令数から固定比で決まる（machine の INSTRUCTIONS_PER_SECOND）ので、
//! 時刻はすべて命令数に換算して扱う。
//!
//! コマンド:
//!
//! ```text
//! tap <x> <y> [押下時間]   down → 押下時間後に up（既定 100ms）
//! down <x> <y> / move <x> <y> / up   ペンの押下・移動・解放
//! key down <名前> / key up <名前>     キーの押下・解放
//! press <名前> [押下時間]  key down → 押下時間後に key up（既定 100ms）
//! rtc <YYYY-MM-DDTHH:MM:SS> RTC（ゲストの時計。ローカル時刻）をこの時刻に合わせる
//! card insert <ファイル>   PC カードのソケットにストレージカード（ディスクイメージ）を挿す
//! card eject [ファイル]    カードを抜く（ファイルを指定するとディスクイメージを書き出す）
//! nic insert [MAC]         イーサネットカードを挿す（MAC は 02:43:52:4c:4e:01 の形。
//!                          省略すると既定の局アドレス）
//! nic eject                イーサネットカードを抜く
//! net rx <16 進>           ネットワークからフレーム（宛先〜データ、FCS なし）が届く
//! shot <ファイル>          画面を保存
//! snap <ファイル>          スナップショットを保存
//! quit                     実行を終了
//! ```
//!
//! 座標は LCD のピクセル座標（左上原点）。範囲やキー名の妥当性はマシン
//! 依存なので、ここでは検査しない（呼び出し側が実行前に検査する）。

use std::fmt;

/// イベントの種類。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    TouchDown,
    TouchMove,
    TouchUp,
    KeyDown,
    KeyUp,
    /// RTC を合わせる（段階3。ブラウザ版が再開時にホストの時刻を渡す。記録される
    /// 入力なので、再生しても同じ命令境界で同じ時刻になる）
    Rtc,
    /// ストレージカードを挿す・抜く（2026-09-29。ディスクイメージの読み書きを伴うので
    /// shot/snap と同じく呼び出し側が適用する。path はイメージのファイル）
    CardInsert,
    CardEject,
    /// イーサネットカードを挿す・抜く（2026-09-30。data は局アドレス 6 バイト）
    NicInsert,
    NicEject,
    /// ネットワークからフレームが届く（2026-09-30。外から来るものはすべて記録する
    /// 入力。data はフレーム）
    NetRx,
    Shot,
    Snap,
    Quit,
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Kind::TouchDown => "down",
            Kind::TouchMove => "move",
            Kind::TouchUp => "up",
            Kind::KeyDown => "key down",
            Kind::KeyUp => "key up",
            Kind::Rtc => "rtc",
            Kind::CardInsert => "card insert",
            Kind::CardEject => "card eject",
            Kind::NicInsert => "nic insert",
            Kind::NicEject => "nic eject",
            Kind::NetRx => "net rx",
            Kind::Shot => "shot",
            Kind::Snap => "snap",
            Kind::Quit => "quit",
        })
    }
}

/// 1 個の入力イベント。tap/press は 2 個のイベントに展開される。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Event {
    /// 実行する時刻（リセットからの命令数）。この命令数に達した時点で適用する
    pub step: u64,
    pub kind: Kind,
    /// TouchDown/TouchMove の座標
    pub x: i64,
    pub y: i64,
    /// KeyDown/KeyUp のキー名
    pub key: String,
    /// Rtc の年月日時分秒
    pub rtc: [i64; 6],
    /// Shot/Snap/CardInsert/CardEject のファイル
    pub path: String,
    /// NicInsert の局アドレス・NetRx のフレーム
    pub data: Vec<u8>,
    /// 元の行番号（エラー表示用。0 は行なし）
    pub line: usize,
}

impl Event {
    pub fn new(step: u64, kind: Kind) -> Event {
        Event {
            step,
            kind,
            x: 0,
            y: 0,
            key: String::new(),
            rtc: [0; 6],
            path: String::new(),
            data: Vec::new(),
            line: 0,
        }
    }
}

/// スクリプトの誤り。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error(pub String);

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

/// tap/press の既定の押下時間（ミリ秒）。100ms は人のタップとして自然な長さで、
/// ドライバのサンプリング間隔（TODO: touch.dll の実測値で見直す）より十分長い値。
pub const DEFAULT_HOLD: u64 = 100;

/// スクリプトを解釈し、時刻順（=記述順）のイベント列を返す。
/// steps_per_second は仮想時間 1 秒あたりの命令数。
pub fn parse(src: &str, steps_per_second: u64) -> Result<Vec<Event>, Error> {
    let mut events = vec![];
    let mut now = 0u64; // 直前のコマンドの終了時刻
    for (i, raw) in src.lines().enumerate() {
        let line_no = i + 1;
        let line = raw.split('#').next().unwrap_or("");
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.is_empty() {
            continue;
        }
        let errf = |msg: String| Error(format!("script line {line_no}: {msg}"));
        if f.len() < 2 {
            return Err(errf("expected <time> <command>".into()));
        }
        let at = parse_time(f[0], now, steps_per_second).map_err(errf)?;
        if at < now {
            return Err(errf(format!(
                "time {} is before the previous command (step {at} < {now})",
                f[0]
            )));
        }
        now = at;
        let ev = |k: Kind| Event {
            line: line_no,
            ..Event::new(at, k)
        };
        let args = &f[2..];
        match f[1] {
            cmd @ ("tap" | "down" | "move") => {
                let max = if cmd == "tap" { 3 } else { 2 };
                if args.len() < 2 || args.len() > max {
                    let hold = if cmd == "tap" { " [hold]" } else { "" };
                    return Err(errf(format!("usage: {cmd} <x> <y>{hold}")));
                }
                let (x, y) = parse_xy(args[0], args[1]).map_err(errf)?;
                let kind = if cmd == "move" {
                    Kind::TouchMove
                } else {
                    Kind::TouchDown
                };
                events.push(Event { x, y, ..ev(kind) });
                if cmd == "tap" {
                    now += hold_steps(&args[2..], steps_per_second).map_err(errf)?;
                    events.push(Event {
                        step: now,
                        ..ev(Kind::TouchUp)
                    });
                }
            }
            "up" => {
                if !args.is_empty() {
                    return Err(errf("usage: up".into()));
                }
                events.push(ev(Kind::TouchUp));
            }
            "key" => {
                if args.len() != 2 || (args[0] != "down" && args[0] != "up") {
                    return Err(errf("usage: key down|up <name>".into()));
                }
                let kind = if args[0] == "up" {
                    Kind::KeyUp
                } else {
                    Kind::KeyDown
                };
                events.push(Event {
                    key: args[1].into(),
                    ..ev(kind)
                });
            }
            "press" => {
                if args.is_empty() || args.len() > 2 {
                    return Err(errf("usage: press <name> [hold]".into()));
                }
                let hold = hold_steps(&args[1..], steps_per_second).map_err(errf)?;
                events.push(Event {
                    key: args[0].into(),
                    ..ev(Kind::KeyDown)
                });
                now += hold;
                events.push(Event {
                    key: args[0].into(),
                    step: now,
                    ..ev(Kind::KeyUp)
                });
            }
            "rtc" => {
                if args.len() != 1 {
                    return Err(errf("usage: rtc YYYY-MM-DDTHH:MM:SS".into()));
                }
                let rtc = parse_datetime(args[0]).map_err(errf)?;
                events.push(Event {
                    rtc,
                    ..ev(Kind::Rtc)
                });
            }
            "card" => {
                let (kind, ok) = match args {
                    ["insert", _] => (Kind::CardInsert, true),
                    ["eject"] | ["eject", _] => (Kind::CardEject, true),
                    _ => (Kind::CardEject, false),
                };
                if !ok {
                    return Err(errf("usage: card insert <file> | card eject [file]".into()));
                }
                events.push(Event {
                    path: args.get(1).map(|s| s.to_string()).unwrap_or_default(),
                    ..ev(kind)
                });
            }
            "nic" => match args {
                ["insert"] | ["insert", _] => {
                    let mac = match args.get(1) {
                        Some(m) => parse_mac(m).map_err(errf)?,
                        None => DEFAULT_MAC.to_vec(),
                    };
                    events.push(Event {
                        data: mac,
                        ..ev(Kind::NicInsert)
                    });
                }
                ["eject"] => events.push(ev(Kind::NicEject)),
                _ => return Err(errf("usage: nic insert [MAC] | nic eject".into())),
            },
            "net" => {
                let ["rx", hex] = args else {
                    return Err(errf("usage: net rx <hex>".into()));
                };
                events.push(Event {
                    data: parse_hex(hex).map_err(errf)?,
                    ..ev(Kind::NetRx)
                });
            }
            cmd @ ("shot" | "snap") => {
                if args.len() != 1 {
                    return Err(errf(format!("usage: {cmd} <file>")));
                }
                let kind = if cmd == "snap" {
                    Kind::Snap
                } else {
                    Kind::Shot
                };
                events.push(Event {
                    path: args[0].into(),
                    ..ev(kind)
                });
            }
            "quit" => {
                if !args.is_empty() {
                    return Err(errf("usage: quit".into()));
                }
                events.push(ev(Kind::Quit));
            }
            cmd => return Err(errf(format!("unknown command {cmd:?}"))),
        }
    }
    Ok(events)
}

/// "YYYY-MM-DDTHH:MM:SS" を年月日時分秒に（範囲は Go の time.Parse と同じく検査する。
/// CLI の --rtc と rtc コマンドが使う）。
pub fn parse_datetime(s: &str) -> Result<[i64; 6], String> {
    let bad = || format!("want YYYY-MM-DDTHH:MM:SS, got {s:?}");
    let b = s.as_bytes();
    if b.len() != 19
        || b[4] != b'-'
        || b[7] != b'-'
        || b[10] != b'T'
        || b[13] != b':'
        || b[16] != b':'
    {
        return Err(bad());
    }
    let num = |r: std::ops::Range<usize>| s[r].parse::<i64>().map_err(|_| bad());
    let v = [
        num(0..4)?,
        num(5..7)?,
        num(8..10)?,
        num(11..13)?,
        num(14..16)?,
        num(17..19)?,
    ];
    let leap = (v[0] % 4 == 0 && v[0] % 100 != 0) || v[0] % 400 == 0;
    let mdays = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    if !(1..=12).contains(&v[1])
        || v[2] < 1
        || v[2] > mdays[(v[1] - 1) as usize]
        || v[3] > 23
        || v[4] > 59
        || v[5] > 59
    {
        return Err(bad());
    }
    Ok(v)
}

/// 既定の局アドレス（イーサネットカード。コアの pccard::ne2000::DEFAULT_MAC と同じ値）。
const DEFAULT_MAC: [u8; 6] = crate::pccard::ne2000::DEFAULT_MAC;

/// "02:43:52:4c:4e:01" を 6 バイトに。
fn parse_mac(s: &str) -> Result<Vec<u8>, String> {
    let v: Result<Vec<u8>, _> = s.split(':').map(|p| u8::from_str_radix(p, 16)).collect();
    match v {
        Ok(v) if v.len() == 6 && s.len() == 17 => Ok(v),
        _ => Err(format!("bad MAC address {s:?} (want 02:43:52:4c:4e:01)")),
    }
}

fn parse_hex(s: &str) -> Result<Vec<u8>, String> {
    if !s.len().is_multiple_of(2) || !s.bytes().all(|c| c.is_ascii_hexdigit()) {
        return Err(format!("bad hex {s:?}"));
    }
    Ok((0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap_or(0))
        .collect())
}

/// 座標（Go の strconv.Atoi と同じく先頭の + を許す。負は不可）。
fn parse_xy(xs: &str, ys: &str) -> Result<(i64, i64), String> {
    let x = xs
        .parse::<i64>()
        .ok()
        .filter(|v| *v >= 0)
        .ok_or_else(|| format!("bad x coordinate {xs:?}"))?;
    let y = ys
        .parse::<i64>()
        .ok()
        .filter(|v| *v >= 0)
        .ok_or_else(|| format!("bad y coordinate {ys:?}"))?;
    Ok((x, y))
}

fn hold_steps(args: &[&str], steps_per_second: u64) -> Result<u64, String> {
    let Some(a) = args.first() else {
        return Ok(DEFAULT_HOLD * steps_per_second / 1000);
    };
    let d = parse_duration(a, steps_per_second)?;
    if d == 0 {
        return Err("hold time must be > 0".into());
    }
    Ok(d)
}

fn parse_time(s: &str, now: u64, steps_per_second: u64) -> Result<u64, String> {
    let (abs, rest) = match (s.len() >= 2, s.strip_prefix('@'), s.strip_prefix('+')) {
        (true, Some(r), _) => (true, r),
        (true, _, Some(r)) => (false, r),
        _ => {
            return Err(format!(
                "time {s:?} must start with @ (absolute) or + (relative)"
            ));
        }
    };
    let d = parse_duration(rest, steps_per_second)?;
    Ok(if abs { d } else { now.wrapping_add(d) })
}

/// "95s"・"1.5s"・"250ms"・"3500000000i" を命令数に換算する。
/// 小数は 10 進のまま整数演算で換算し（浮動小数の丸めで命令数が環境依存に
/// ならないように）、端数の命令は切り捨てる。
pub fn parse_duration(s: &str, steps_per_second: u64) -> Result<u64, String> {
    // 1 単位 = unit_num/unit_den 命令
    let (num, unit_num, unit_den) = if let Some(n) = s.strip_suffix("ms") {
        (n, steps_per_second, 1000u64)
    } else if let Some(n) = s.strip_suffix('s') {
        (n, steps_per_second, 1)
    } else if let Some(n) = s.strip_suffix('i') {
        (n, 1, 1)
    } else {
        return Err(format!("duration {s:?} needs a unit (s, ms or i)"));
    };
    let (int_part, frac_part) = match num.split_once('.') {
        Some((i, f)) => (i, Some(f)),
        None => (num, None),
    };
    let bad = || format!("bad number in duration {s:?}");
    if int_part.is_empty() || frac_part == Some("") {
        return Err(bad());
    }
    if frac_part.is_some() && unit_den == 1 && unit_num == 1 {
        return Err(format!("instruction count {s:?} must be an integer"));
    }
    let frac = frac_part.unwrap_or("");
    let digits = format!("{int_part}{frac}");
    if !digits.bytes().all(|c| c.is_ascii_digit()) {
        return Err(bad());
    }
    if frac.len() > 9 {
        return Err(format!("too many decimal places in {s:?}"));
    }
    let v: u64 = digits.parse().map_err(|_| bad())?;
    let den = unit_den * 10u64.pow(frac.len() as u32);
    // v × unit_num / den。オーバーフローを避けるため商と余りに分ける。
    let (q, r) = (v / den, v % den);
    let hi = q
        .checked_mul(unit_num)
        .ok_or_else(|| format!("duration {s:?} too large"))?;
    // r×unit_num は 64 ビットを超え得るので 128 ビットで計算する
    // （r < den なので商は unit_num 未満に収まる）。
    let frac_steps = (r as u128 * unit_num as u128 / den as u128) as u64;
    Ok(hi + frac_steps)
}

/// イベント列を、parse で読み戻せるスクリプトとして書く（操作の記録の
/// 書き出し用。ユーザー確認済み 2026-09）。
///
/// 時刻はすべて絶対命令数（@<n>i）で書く。s/ms や相対時刻は換算の丸めが
/// 入り得るので使わない（記録した命令境界をそのまま再現するため）。
/// tap/press には畳まず、down/up・key down/up のまま書く。
/// header の各行は "# " を付けたコメントとして先頭に書く。
pub fn format(header: &[String], events: &[Event]) -> Result<String, Error> {
    use std::fmt::Write;
    let mut b = String::new();
    for h in header {
        for l in h.split('\n') {
            writeln!(b, "# {l}").unwrap();
        }
    }
    let mut prev = 0;
    for (i, ev) in events.iter().enumerate() {
        if ev.step < prev {
            return Err(Error(format!(
                "script: event {i} (step {}) is before the previous one (step {prev})",
                ev.step
            )));
        }
        prev = ev.step;
        write!(b, "@{}i {}", ev.step, ev.kind).unwrap();
        match ev.kind {
            Kind::TouchDown | Kind::TouchMove => {
                if ev.x < 0 || ev.y < 0 {
                    return Err(Error(format!(
                        "script: event {i}: negative coordinate ({},{})",
                        ev.x, ev.y
                    )));
                }
                write!(b, " {} {}", ev.x, ev.y).unwrap();
            }
            Kind::KeyDown | Kind::KeyUp => {
                check_word(&ev.key).map_err(|e| Error(format!("script: event {i}: key {e}")))?;
                write!(b, " {}", ev.key).unwrap();
            }
            Kind::Shot | Kind::Snap | Kind::CardInsert => {
                check_word(&ev.path).map_err(|e| Error(format!("script: event {i}: path {e}")))?;
                write!(b, " {}", ev.path).unwrap();
            }
            Kind::CardEject => {
                if !ev.path.is_empty() {
                    check_word(&ev.path)
                        .map_err(|e| Error(format!("script: event {i}: path {e}")))?;
                    write!(b, " {}", ev.path).unwrap();
                }
            }
            Kind::Rtc => {
                let [y, mo, d, h, mi, se] = ev.rtc;
                write!(b, " {y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{se:02}").unwrap();
            }
            Kind::NicInsert => {
                let m = &ev.data;
                if m.len() != 6 {
                    return Err(Error(format!("script: event {i}: bad MAC address")));
                }
                write!(
                    b,
                    " {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
                    m[0], m[1], m[2], m[3], m[4], m[5]
                )
                .unwrap();
            }
            Kind::NetRx => {
                if ev.data.is_empty() {
                    return Err(Error(format!("script: event {i}: empty frame")));
                }
                b.push(' ');
                for x in &ev.data {
                    write!(b, "{x:02x}").unwrap();
                }
            }
            Kind::TouchUp | Kind::NicEject | Kind::Quit => {}
        }
        b.push('\n');
    }
    Ok(b)
}

/// 1 語として書けるか（空白・"#" を含むと parse で分割・コメント扱いされて
/// 読み戻せない）。
fn check_word(s: &str) -> Result<(), String> {
    if s.is_empty() || s.contains([' ', '\t', '\r', '\n', '#']) {
        return Err(format!("{s:?} cannot be written as a single word"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// テストでは 1 秒 = 1000 命令にして読みやすくする。
    const SPS: u64 = 1000;

    #[test]
    fn parse_duration_table() {
        for (s, sps, want) in [
            ("1s", SPS, Some(1000)),
            ("1.5s", SPS, Some(1500)),
            ("250ms", SPS, Some(250)),
            ("0.5ms", SPS, Some(0)), // 端数命令は切り捨て
            ("12345i", SPS, Some(12345)),
            ("0s", SPS, Some(0)),
            // 実機構成の換算（135.2M 命令/秒）
            ("95s", 135_200_000, Some(12_844_000_000)),
            ("1ms", 135_200_000, Some(135_200)),
            ("0.001s", 135_200_000, Some(135_200)),
            ("2.123456789s", 135_200_000, Some(287_091_357)), // 287091357.8 の切り捨て
            ("1.5i", SPS, None),
            ("10", SPS, None),
            ("s", SPS, None),
            ("1.s", SPS, None),
            (".5s", SPS, None),
            ("-1s", SPS, None),
            ("1e3ms", SPS, None),
            ("99999999999999999999s", SPS, None),
        ] {
            assert_eq!(parse_duration(s, sps).ok(), want, "{s}");
        }
    }

    #[test]
    fn parse_script() {
        let src = "
# Start メニューを開いて Calendar を起動する
@95s    tap 30 310          # 既定 100ms 押下
+2s     shot start.png
+500ms  down 120 160
+50ms   move 130 170
+50ms   up
@100s   key down Enter
+100ms  key up Enter
+1s     press SoftL 200ms
+0s     tap 1 2 1ms
@200000i snap s.snap
+1s     quit
";
        let got = parse(src, SPS).unwrap();
        let e = |step, kind, line| Event {
            line,
            ..Event::new(step, kind)
        };
        let xy = |step, kind, x, y, line| Event {
            x,
            y,
            ..e(step, kind, line)
        };
        let key = |step, kind, k: &str, line| Event {
            key: k.into(),
            ..e(step, kind, line)
        };
        let path = |step, kind, p: &str, line| Event {
            path: p.into(),
            ..e(step, kind, line)
        };
        let want = vec![
            xy(95000, Kind::TouchDown, 30, 310, 3),
            e(95100, Kind::TouchUp, 3),
            path(97100, Kind::Shot, "start.png", 4), // tap の終了から +2s
            xy(97600, Kind::TouchDown, 120, 160, 5),
            xy(97650, Kind::TouchMove, 130, 170, 6),
            e(97700, Kind::TouchUp, 7),
            key(100000, Kind::KeyDown, "Enter", 8),
            key(100100, Kind::KeyUp, "Enter", 9),
            key(101100, Kind::KeyDown, "SoftL", 10),
            key(101300, Kind::KeyUp, "SoftL", 10),
            xy(101300, Kind::TouchDown, 1, 2, 11),
            e(101301, Kind::TouchUp, 11),
            path(200000, Kind::Snap, "s.snap", 12),
            e(201000, Kind::Quit, 13),
        ];
        assert_eq!(got, want);
    }

    #[test]
    fn parse_errors() {
        for (src, want) in [
            ("95s tap 1 2", "must start with @"),
            ("@1s", "expected <time> <command>"),
            ("@2s up\n@1s up", "line 2: time @1s is before"),
            ("@1s tap 1", "usage: tap"),
            ("@1s tap -1 2", "bad x"),
            ("@1s tap 1 2 0ms", "hold time must be > 0"),
            ("@1s key press A", "usage: key"),
            ("@1s jump", "unknown command \"jump\""),
            ("@1s shot", "usage: shot"),
            ("@1s up now", "usage: up"),
            ("@1s press", "usage: press"),
            ("@1s tap 1 2 5", "needs a unit"),
            // tap は押下時間の後が終了なので、同じ時刻への絶対指定は後戻りになる
            ("@1s tap 1 2\n@1s up", "before the previous command"),
        ] {
            let err = parse(src, SPS).unwrap_err();
            assert!(err.0.contains(want), "{src:?}: {err}");
        }
    }

    /// format → parse の往復でイベント列（行番号以外）が一致すること。
    #[test]
    fn format_round_trip() {
        let e = |step, kind| Event::new(step, kind);
        let input = vec![
            Event {
                x: 20,
                y: 10,
                ..e(3600000000, Kind::TouchDown)
            },
            Event {
                x: 21,
                y: 11,
                ..e(3600000001, Kind::TouchMove)
            },
            e(3613520000, Kind::TouchUp),
            Event {
                key: "Right".into(),
                ..e(3613520000, Kind::KeyDown)
            },
            Event {
                key: "Right".into(),
                ..e(3700000000, Kind::KeyUp)
            },
            Event {
                path: "out/a.png".into(),
                ..e(3700000000, Kind::Shot)
            },
            Event {
                path: "a.snap".into(),
                ..e(3700000000, Kind::Snap)
            },
            Event {
                rtc: [2026, 9, 29, 7, 4, 5],
                ..e(3700000000, Kind::Rtc)
            },
            e(3700000001, Kind::Quit),
        ];
        let text = format(&["recorded by test".into(), "start: x.snap".into()], &input).unwrap();
        assert!(
            text.starts_with("# recorded by test\n# start: x.snap\n@3600000000i down 20 10\n"),
            "{text}"
        );
        let mut out = parse(&text, 135_200_000).unwrap();
        for ev in &mut out {
            ev.line = 0;
        }
        assert_eq!(out, input);
    }

    #[test]
    fn format_errors() {
        let e = |step, kind| Event::new(step, kind);
        for ev in [
            vec![e(2, Kind::TouchUp), e(1, Kind::TouchUp)],
            vec![Event {
                x: -1,
                ..e(0, Kind::TouchDown)
            }],
            vec![Event {
                key: "a b".into(),
                ..e(0, Kind::KeyDown)
            }],
            vec![Event {
                path: "".into(),
                ..e(0, Kind::Shot)
            }],
            vec![Event {
                path: "x#y".into(),
                ..e(0, Kind::Snap)
            }],
        ] {
            assert!(format(&[], &ev).is_err(), "{ev:?}");
        }
    }

    /// rtc コマンド: 日時の書式と範囲（閏年を含む）を検査する。
    #[test]
    fn rtc_command() {
        let evs = parse("@5i rtc 2024-02-29T23:59:59\n", SPS).unwrap();
        assert_eq!(evs[0].kind, Kind::Rtc);
        assert_eq!(evs[0].rtc, [2024, 2, 29, 23, 59, 59]);
        for bad in [
            "@0i rtc",
            "@0i rtc 2023-02-29T00:00:00",
            "@0i rtc 2024-13-01T00:00:00",
            "@0i rtc 2024-01-01T24:00:00",
            "@0i rtc 2024-01-01 00:00:00",
        ] {
            assert!(parse(bad, SPS).is_err(), "{bad}");
        }
    }

    /// 基準シナリオのスクリプトが読めること（絶対命令数だけ・入力だけ）。
    #[test]
    fn golden_scripts() {
        // テストデータはコンパイル時に埋め込む（wasm32-wasip1 のテストではファイルを読めない）。
        for (name, src) in [
            (
                "today-calendar",
                include_str!("../../../testdata/golden/scenarios/today-calendar.script"),
            ),
            (
                "taps-5",
                include_str!("../../../testdata/golden/scenarios/taps-5.script"),
            ),
        ] {
            let evs = parse(src, 135_200_000).unwrap();
            assert!(!evs.is_empty());
            assert!(
                evs.iter()
                    .all(|e| !matches!(e.kind, Kind::Shot | Kind::Snap | Kind::Quit)),
                "{name}"
            );
        }
    }
}
