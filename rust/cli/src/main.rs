//! CErulean のネイティブの開発用 CLI（Go 版の cmd/cerulean の run・info に当たる）。
//! フラグ名と出力の書式は Rust 版で決めている（Go 版との互換は要件ではない）。
//! 一致確認の出力（--result・--trace-hash）の中身は testdata/golden/README.md が正。

mod result;

use std::io::Write;
use std::process::ExitCode;
use std::time::Instant;

use cerulean_core::arm;
use cerulean_core::emu::{self, RunError, Session};
use cerulean_core::loader;
use cerulean_core::script::{self, Event, Kind};
use cerulean_core::smdk2410::{INSTRUCTIONS_PER_SECOND, Machine};
use cerulean_core::snapshot;

use result::{ResultWriter, Stop, TraceHasher, UartTap};

const USAGE: &str = "\
Usage:
  cerulean info <image>            イメージの情報を表示する
  cerulean snapdump <snap> [snap2] スナップショットのチャンクの一覧（2 つなら比較）
  cerulean run [options] <image>   イメージをリセットから実行する
  cerulean run --snap-load F [options] [image]
                                   スナップショットから再開する（image を渡すと照合する）

run options:
  --rtc YYYY-MM-DDTHH:MM:SS  RTC の初期時刻（年月日時分秒をそのまま使う。
                             既定はホストの現在時刻の UTC。TODO: ローカル時刻）
  --max-steps N         N 命令で止める（0 = 無制限）
  --script F            入力スクリプト（書式は script モジュールのコメント）
  --history N           停止時に直前 N 命令の PC を表示する（既定 16、0 = 無効）
  --trace               実行した命令の PC と命令語を逐一表示する
  --trace-from N        --trace の表示を N 命令目から始める
  --sample N            N 命令ごとに PC・CPSR を 1 行表示する（停滞箇所の調査用）
  --fb-out F.png        停止時に画面を PNG に書く
  --fb-every N          --fb-out と併用。N 命令ごとに F-<命令数>.png として連番で書く
  --watch LO[-HI]       物理アドレス範囲へのアクセスを表示する（複数可。1 命令ずつ進む）
  --stats               停止時に実行速度を表示する
  --no-idle-skip        アイドルループのスキップを無効にする（結果は同じ。--trace 中は自動で無効）
  --snap-save F@T       時刻 T（例 3600000000i・95s）に全状態を F に保存する（無圧縮）
  --snap-load F         スナップショット F から再開する（命令数は保存時点から継続）
  --result F            一致確認用の結果を JSON Lines で F に書く（停止時と --checkpoint）
  --checkpoint N        --result に N 命令目の時点の結果も書く（複数可）
  --trace-hash N        N 命令ごとに命令数と CPU 状態のダンプの SHA-256 を書く
  --trace-hash-ram N    N 命令ごとに RAM の SHA-256 も加える
  --trace-hash-out F    --trace-hash の出力先（既定は標準エラー）
  --quiet-uart          UART1 の出力を標準出力に流さない
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let r = match args.first().map(String::as_str) {
        Some("info") => cmd_info(&args[1..]),
        Some("run") => cmd_run(&args[1..]),
        Some("snapdump") => cmd_snapdump(&args[1..]),
        _ => {
            eprint!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    match r {
        Ok(code) => code,
        Err(e) => {
            eprintln!("cerulean: {e}");
            ExitCode::from(1)
        }
    }
}

fn read_image(path: &str) -> Result<loader::Image, String> {
    let data = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
    // .nb0 のロード先: SMDK2410 の SDRAM 先頭。
    // TODO: 実イメージで .nb0 のベースを確認する（Go 版と同じ仮定）。
    loader::load(&data, path, 0x30000000).map_err(|e| e.to_string())
}

fn cmd_info(args: &[String]) -> Result<ExitCode, String> {
    let [path] = args else {
        return Err("usage: cerulean info <image>".into());
    };
    let img = read_image(path)?;
    println!("format:  {}", img.format);
    println!("start:   {:08X}", img.start);
    println!("length:  {:08X} ({} bytes)", img.length, img.length);
    println!("entry:   {:08X}", img.entry);
    if !img.records.is_empty() {
        println!("records: {}", img.records.len());
        for (i, r) in img.records.iter().enumerate() {
            println!(
                "  [{i:3}] addr={:08X} len={:8} checksum={:08X}",
                r.addr, r.len, r.checksum
            );
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// run のオプション。
#[derive(Default)]
struct RunOpts {
    image: String,
    rtc: Option<[i64; 6]>,
    max_steps: u64,
    script: Option<String>,
    history: usize,
    trace: bool,
    trace_from: u64,
    watches: Vec<(u32, u32)>,
    stats: bool,
    result: Option<String>,
    checkpoints: Vec<u64>,
    trace_hash: u64,
    trace_hash_ram: u64,
    trace_hash_out: Option<String>,
    quiet_uart: bool,
    no_idle_skip: bool,
    snap_save: Option<(String, u64)>,
    snap_load: Option<String>,
    sample: u64,
    fb_out: Option<String>,
    fb_every: u64,
}

fn parse_u64(s: &str) -> Result<u64, String> {
    let r = match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        Some(h) => u64::from_str_radix(h, 16),
        None => s.parse(),
    };
    r.map_err(|_| format!("bad number {s:?}"))
}

/// "YYYY-MM-DDTHH:MM:SS" を年月日時分秒に（範囲は Go の time.Parse と同じく検査する）。
fn parse_rtc(s: &str) -> Result<[i64; 6], String> {
    let bad = || format!("--rtc: want YYYY-MM-DDTHH:MM:SS, got {s:?}");
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

/// "lo" または "lo-hi"。
fn parse_range(s: &str) -> Result<(u32, u32), String> {
    let (lo, hi) = s.split_once('-').unwrap_or((s, s));
    let (lo, hi) = (parse_u64(lo)?, parse_u64(hi)?);
    if lo > hi || hi > u32::MAX as u64 {
        return Err(format!("--watch: bad range {s:?}"));
    }
    Ok((lo as u32, hi as u32))
}

fn parse_run_opts(args: &[String]) -> Result<RunOpts, String> {
    let mut o = RunOpts {
        history: 16,
        ..Default::default()
    };
    let mut it = args.iter();
    let mut image = None;
    while let Some(a) = it.next() {
        let mut val = || {
            it.next()
                .cloned()
                .ok_or_else(|| format!("{a} needs a value"))
        };
        match a.as_str() {
            "--rtc" => o.rtc = Some(parse_rtc(&val()?)?),
            "--max-steps" => o.max_steps = parse_u64(&val()?)?,
            "--script" => o.script = Some(val()?),
            "--history" => o.history = parse_u64(&val()?)? as usize,
            "--trace" => o.trace = true,
            "--trace-from" => o.trace_from = parse_u64(&val()?)?,
            "--watch" => o.watches.push(parse_range(&val()?)?),
            "--stats" => o.stats = true,
            "--result" => o.result = Some(val()?),
            "--checkpoint" => o.checkpoints.push(parse_u64(&val()?)?),
            "--trace-hash" => o.trace_hash = parse_u64(&val()?)?,
            "--trace-hash-ram" => o.trace_hash_ram = parse_u64(&val()?)?,
            "--trace-hash-out" => o.trace_hash_out = Some(val()?),
            "--quiet-uart" => o.quiet_uart = true,
            "--no-idle-skip" => o.no_idle_skip = true,
            "--snap-save" => {
                let v = val()?;
                let (f, t) = v.rsplit_once('@').ok_or("--snap-save: want FILE@TIME")?;
                let t = script::parse_duration(t, INSTRUCTIONS_PER_SECOND)
                    .map_err(|e| format!("--snap-save: {e}"))?;
                o.snap_save = Some((f.to_string(), t));
            }
            "--snap-load" => o.snap_load = Some(val()?),
            "--sample" => o.sample = parse_u64(&val()?)?,
            "--fb-out" => o.fb_out = Some(val()?),
            "--fb-every" => o.fb_every = parse_u64(&val()?)?,
            s if s.starts_with("--") => return Err(format!("unknown option {s}\n{USAGE}")),
            s => {
                if image.replace(s.to_string()).is_some() {
                    return Err(format!("more than one image\n{USAGE}"));
                }
            }
        }
    }
    o.image = image.unwrap_or_default();
    if o.image.is_empty() && o.snap_load.is_none() {
        return Err(format!("no image\n{USAGE}"));
    }
    if o.snap_load.is_some() && o.rtc.is_some() {
        return Err(
            "--rtc cannot be used with --snap-load (the RTC state comes from the snapshot)".into(),
        );
    }
    if o.fb_every != 0 && o.fb_out.is_none() {
        return Err("--fb-every requires --fb-out".into());
    }
    if !o.checkpoints.is_empty() && o.result.is_none() {
        return Err("--checkpoint requires --result".into());
    }
    Ok(o)
}

/// ホストの現在時刻（UTC）の年月日時分秒。
/// TODO: Go 版はローカル時刻を使った。std にはタイムゾーンがないので、ローカル
/// 時刻にするには依存（または OS の API）が要る。一致確認では --rtc を固定するので
/// 影響しない。
fn host_now_utc() -> [i64; 6] {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    let days = secs.div_euclid(86400);
    let rem = secs.rem_euclid(86400);
    // 暦の変換はコアの RTC と同じ式（Howard Hinnant の civil_from_days）。
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    [y, m, d, rem / 3600, rem / 60 % 60, rem % 60]
}

/// pc の命令語（Thumb ならハーフワード）と逆アセンブルの表示。状態は変えない。
fn instr_text(m: &mut Machine, pc: u32, thumb: bool) -> String {
    let Some(w0) = m.peek32(pc) else {
        return "????????".into();
    };
    if !thumb {
        return format!("{w0:08X}  {}", arm::disasm(w0, pc));
    }
    // BL の対を表示するため直後のハーフワードも読む。
    let (hw, next) = if pc & 2 == 0 {
        (w0 & 0xFFFF, w0 >> 16)
    } else {
        (w0 >> 16, m.peek32(pc.wrapping_add(4)).unwrap_or(0) & 0xFFFF)
    };
    format!("    {hw:04X}  {}", arm::disasm_thumb(hw, next, pc))
}

/// 画面を PNG に書く（PNG 化・ファイル出力は CLI の責務。コアは RGBA を返すだけ）。
fn write_png(m: &mut Machine, path: &str) -> Result<(), String> {
    let (f, cfg) = m.frame().map_err(|e| e.to_string())?;
    let file = std::fs::File::create(path).map_err(|e| format!("{path}: {e}"))?;
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), f.width, f.height);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let mut w = enc.write_header().map_err(|e| format!("{path}: {e}"))?;
    w.write_image_data(&f.rgba)
        .map_err(|e| format!("{path}: {e}"))?;
    w.finish().map_err(|e| format!("{path}: {e}"))?;
    eprintln!("cerulean: wrote {path} ({cfg})");
    Ok(())
}

/// "shot.png" → "shot-000123456789.png"（命令数を 12 桁で埋め、ファイル名順 = 時系列にする）。
fn numbered_path(path: &str, steps: u64) -> String {
    match path.rsplit_once('.') {
        Some((stem, ext)) if !ext.contains('/') => format!("{stem}-{steps:012}.{ext}"),
        _ => format!("{path}-{steps:012}"),
    }
}

/// ファイル全体の SHA-256（スナップショットのイメージ ID）。
fn file_sha256(path: &str) -> Result<String, String> {
    let data = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
    Ok(result::sha256_hex(&data))
}

/// スナップショットを一時ファイルに書いてから置き換える（途中で失敗しても
/// 壊れたスナップショットが残らないように）。
fn save_snapshot(m: &mut Machine, path: &str, image_id: &str) -> Result<(), String> {
    let tmp = format!("{path}.tmp{}", std::process::id());
    let r = (|| {
        let f = std::fs::File::create(&tmp).map_err(|e| format!("{tmp}: {e}"))?;
        let mut w = std::io::BufWriter::new(f);
        m.save_snapshot(&mut w, image_id)
            .map_err(|e| e.to_string())?;
        w.flush().map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, path).map_err(|e| format!("{path}: {e}"))
    })();
    if r.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    r?;
    eprintln!("cerulean: saved snapshot {path} at step {}", m.steps());
    Ok(())
}

fn cmd_run(args: &[String]) -> Result<ExitCode, String> {
    let o = parse_run_opts(args)?;
    let mut m = Machine::new();
    for &(lo, hi) in &o.watches {
        m.add_watch(lo, hi);
    }
    let mut image_id = if o.image.is_empty() {
        String::new()
    } else {
        file_sha256(&o.image)?
    };
    if let Some(path) = &o.snap_load {
        // 監視は読み込みより前に登録する（TLB は監視中なら RAM を直接持たない形で復元される）。
        let f = std::fs::File::open(path).map_err(|e| format!("{path}: {e}"))?;
        let id = m
            .load_snapshot(std::io::BufReader::new(f))
            .map_err(|e| format!("--snap-load {path}: {e}"))?;
        if !image_id.is_empty() && id != image_id {
            return Err(format!(
                "--snap-load: snapshot was taken with a different image (sha256 {id}, {} is {image_id})",
                o.image
            ));
        }
        image_id = id;
        eprintln!(
            "cerulean: {}: resumed from {path} at step {}, PC {:08X}",
            m.name(),
            m.steps(),
            m.cpu.pc()
        );
    } else {
        let img = read_image(&o.image)?;
        m.load_image(&img).map_err(|e| e.to_string())?;
        let t = o.rtc.unwrap_or_else(host_now_utc);
        m.set_rtc(t[0], t[1], t[2], t[3], t[4], t[5]);
        m.reset();
        eprintln!(
            "cerulean: {}: loaded {} image, entry {:08X} (PA {:08X})",
            m.name(),
            img.format,
            img.entry,
            m.cpu.pc()
        );
    }
    m.cpu.set_history(o.history);
    // トレースはスキップした命令を表示できないので、アイドルスキップを切る
    // （監視中は MMU が RAM を直接持たないので元々スキップされない）。
    if o.no_idle_skip || o.trace {
        m.set_idle_skip(false);
    }

    let mut events = vec![];
    if let Some(path) = &o.script {
        let src = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
        events =
            script::parse(&src, INSTRUCTIONS_PER_SECOND).map_err(|e| format!("{path}: {e}"))?;
        for ev in &events {
            emu::validate(&m, ev).map_err(|e| format!("script line {}: {e}", ev.line))?;
        }
    }
    if let Some((f, t)) = &o.snap_save {
        // 同じ時刻のイベントの後に保存する（Go の -snap-save と同じ）。
        let at = events
            .iter()
            .position(|e| e.step > *t)
            .unwrap_or(events.len());
        events.insert(
            at,
            Event {
                path: f.clone(),
                ..Event::new(*t, Kind::Snap)
            },
        );
    }
    // 再開点より前のイベントは、保存前の実行で適用済みとみなして読み飛ばす
    // （同じスクリプトを再開に使い回せるように）。
    let start = m.steps();
    let skip = events.iter().take_while(|e| e.step < start).count();
    if skip > 0 {
        eprintln!("cerulean: skipped {skip} script events before the resume point (step {start})");
    }
    let mut sess = Session::new();
    sess.schedule(events.into_iter().skip(skip));

    let mut results = match &o.result {
        Some(p) => Some(ResultWriter {
            out: std::fs::File::create(p).map_err(|e| format!("{p}: {e}"))?,
        }),
        None => None,
    };
    let mut hasher = if o.trace_hash != 0 || o.trace_hash_ram != 0 {
        let out: Box<dyn Write> = match &o.trace_hash_out {
            Some(p) => Box::new(std::io::BufWriter::new(
                std::fs::File::create(p).map_err(|e| format!("{p}: {e}"))?,
            )),
            None => Box::new(std::io::stderr()),
        };
        Some(TraceHasher {
            out,
            every: o.trace_hash,
            ram_every: o.trace_hash_ram,
        })
    } else {
        None
    };

    let mut uart = UartTap::new();
    let mut stdout = std::io::stdout();
    let io = |e: std::io::Error| e.to_string();
    // UART1 の出力を取り出し、ハッシュを取りながら標準出力に流す。
    let mut drain_uart = |m: &mut Machine, uart: &mut UartTap| {
        let b = m.take_uart1();
        if !b.is_empty() {
            uart.feed(&b);
            if !o.quiet_uart {
                let _ = stdout.write_all(&b);
                let _ = stdout.flush();
            }
        }
    };
    let mut apply = |m: &mut Machine, ev: &Event| -> Result<bool, String> {
        match ev.kind {
            Kind::Quit => Ok(true),
            // TODO(段階1): 画面の保存（PNG）とスナップショット。
            Kind::Snap => save_snapshot(m, &ev.path, &image_id).map(|_| false),
            Kind::Shot => write_png(m, &ev.path).map(|_| false),
            _ => emu::apply_input(m, ev),
        }
    };

    let started = Instant::now();
    let stop = loop {
        // 今の命令数に予定されたイベントを先に適用する（トレース表示を適用後の
        // 状態で出すため）。
        let steps = m.steps();
        let mut r = sess.run(&mut m, steps, &mut apply);
        if matches!(r, Ok(false)) && o.trace && steps >= o.trace_from {
            let (pc, thumb) = (m.cpu.pc(), m.cpu.thumb());
            eprintln!("{steps:12}  PC={pc:08X}  {}", instr_text(&mut m, pc, thumb));
        }
        let mut target = u64::MAX;
        if o.sample != 0 {
            target = target.min(result::next_multiple(steps, o.sample));
        }
        if o.fb_every != 0 {
            target = target.min(result::next_multiple(steps, o.fb_every));
        }
        if o.max_steps != 0 {
            target = target.min(o.max_steps);
        }
        // 結果・ハッシュを取る命令数ちょうどで止まる。
        if let Some(h) = &hasher {
            target = target.min(h.next(steps));
        }
        if let Some(c) = o.checkpoints.iter().filter(|&&c| c > steps).min() {
            target = target.min(*c);
        }
        if o.trace {
            target = if steps >= o.trace_from {
                steps + 1
            } else {
                target.min(o.trace_from)
            };
        }
        if !o.watches.is_empty() {
            target = steps + 1; // 監視の表示に命令数と PC を付けるため 1 命令ずつ
        }
        // 少なくとも 1 命令は進める（--max-steps が今の命令数以下の場合など）。
        target = target.max(steps + 1);
        if matches!(r, Ok(false)) {
            r = sess.run(&mut m, target, &mut apply);
        }
        drain_uart(&mut m, &mut uart);
        for ev in m.take_watch_log() {
            eprintln!(
                "{:12}  watch {}{} {:<10} PA={:08X} v={:08X}  (PC after={:08X})",
                m.steps(),
                if ev.write { "W" } else { "R" },
                ev.size * 8,
                ev.region,
                ev.addr,
                ev.value,
                m.cpu.pc()
            );
        }
        match r {
            Err(RunError::Event { event, err }) => {
                eprintln!("cerulean: script line {}: {err}", event.line);
                break Stop::EventError;
            }
            Err(RunError::Stop(e)) => {
                eprintln!("cerulean: stopped after {} steps: {e}", m.steps());
                break Stop::Emu(e);
            }
            Ok(true) => {
                eprintln!(
                    "cerulean: stopped after {} steps at PC={:08X} (script quit)",
                    m.steps(),
                    m.cpu.pc()
                );
                break Stop::Quit;
            }
            Ok(false) => {}
        }
        // その命令数に予定された入力イベントを適用する前の状態を書く
        // （イベントは次の周の先頭で適用される）。
        let steps = m.steps();
        if o.sample != 0 && steps.is_multiple_of(o.sample) {
            eprintln!(
                "{steps:12}  sample PC={:08X} CPSR={:08X}",
                m.cpu.pc(),
                m.cpu.cpsr()
            );
        }
        if o.fb_every != 0
            && steps.is_multiple_of(o.fb_every)
            && let Some(f) = &o.fb_out
            && let Err(e) = write_png(&mut m, &numbered_path(f, steps))
        {
            eprintln!("cerulean: {e}");
        }
        if let Some(h) = &mut hasher {
            h.emit(&m).map_err(io)?;
        }
        if o.checkpoints.contains(&steps)
            && let Some(w) = &mut results
        {
            w.write(&mut m, &uart, "checkpoint", None).map_err(io)?;
        }
        if o.max_steps != 0 && steps >= o.max_steps {
            eprintln!(
                "cerulean: stopped after {steps} steps at PC={:08X} (max-steps)",
                m.cpu.pc()
            );
            break Stop::MaxSteps;
        }
    };
    if let Some(h) = &mut hasher {
        h.out.flush().map_err(io)?;
    }
    if let Some(w) = &mut results {
        w.write(&mut m, &uart, "stop", Some(&stop)).map_err(io)?;
    }
    if let Some(f) = &o.fb_out
        && let Err(e) = write_png(&mut m, f)
    {
        eprintln!("cerulean: {e}");
    }
    report(&mut m, &o, started, &stop);
    Ok(match stop {
        Stop::MaxSteps | Stop::Quit => ExitCode::SUCCESS,
        _ => ExitCode::from(1),
    })
}

/// 停止時の共通の表示（PC の変換・レジスタ・命令履歴・速度）。
fn report(m: &mut Machine, o: &RunOpts, started: Instant, _stop: &Stop) {
    let pc = m.cpu.pc();
    match m.translate(pc) {
        Ok(pa) => eprintln!("  PC VA {pc:08X} -> PA {pa:08X}"),
        Err(e) => eprintln!("  PC VA {pc:08X} -> {e}"),
    }
    for i in 0..16 {
        eprint!("  r{i:<2}={:08X}", m.cpu.reg(i));
        if i % 4 == 3 {
            eprintln!();
        }
    }
    if o.history > 0 {
        let h = m.cpu.history();
        eprintln!("last {} instructions:", h.len());
        for (pc, thumb) in h {
            eprintln!("  PC={pc:08X}  {}", instr_text(m, pc, thumb));
        }
    }
    if o.stats {
        let el = started.elapsed().as_secs_f64();
        let n = m.steps();
        eprintln!(
            "cerulean: {n} steps in {el:.2}s ({:.1}M steps/s, {:.2}x real time, idle-skipped {:.1}%)",
            n as f64 / el / 1e6,
            n as f64 / INSTRUCTIONS_PER_SECOND as f64 / el,
            100.0 * m.idle_skipped() as f64 / n.max(1) as f64
        );
    }
}

/// snapdump: スナップショットのチャンクの一覧（名前・版数・長さ・SHA-256）。
/// 2 つ渡すと、チャンクごとに同じかを表示する（食い違いの調査用）。
fn cmd_snapdump(args: &[String]) -> Result<ExitCode, String> {
    if args.is_empty() || args.len() > 2 {
        return Err("usage: cerulean snapdump <snap> [snap2]".into());
    }
    type Chunks = Vec<(String, u16, usize, String)>;
    let list = |path: &str| -> Result<(snapshot::Header, Chunks), String> {
        let f = std::fs::File::open(path).map_err(|e| format!("{path}: {e}"))?;
        let mut r = snapshot::Reader::new(std::io::BufReader::new(f))
            .map_err(|e| format!("{path}: {e}"))?;
        let mut v = vec![];
        while let Some(c) = r.next_chunk().map_err(|e| format!("{path}: {e}"))? {
            v.push((
                c.name.clone(),
                c.version,
                c.body.len(),
                result::sha256_hex(&c.body),
            ));
        }
        Ok((r.header, v))
    };
    let (ha, a) = list(&args[0])?;
    println!("{}: machine={} image={}", args[0], ha.machine, ha.image_id);
    let Some(other) = args.get(1) else {
        for (name, v, len, sum) in &a {
            println!("  {name:<18} v{v} {len:>10} bytes  sha256 {sum}");
        }
        return Ok(ExitCode::SUCCESS);
    };
    let (hb, b) = list(other)?;
    println!("{other}: machine={} image={}", hb.machine, hb.image_id);
    let mut same = true;
    for (name, v, len, sum) in &a {
        match b.iter().find(|c| &c.0 == name) {
            Some(c) if c.1 == *v && c.3 == *sum => println!("  same  {name}"),
            Some(c) => {
                same = false;
                println!("  DIFF  {name} (v{v} {len} bytes / v{} {} bytes)", c.1, c.2);
            }
            None => {
                same = false;
                println!("  ONLY  {name} (in {})", args[0]);
            }
        }
    }
    for c in b.iter().filter(|c| !a.iter().any(|x| x.0 == c.0)) {
        same = false;
        println!("  ONLY  {} (in {other})", c.0);
    }
    Ok(if same {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}
