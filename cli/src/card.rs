//! `cerulean card`: ストレージカードのディスクイメージ（MBR＋FAT）を作る・中身を
//! 出し入れする（2026-09-29 ユーザー決定: カードに簡単にデータを入れられること。
//! 「抜く → イメージを編集 → 挿す」の「編集」をホストで行う）。

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::SystemTime;

use cerulean_fat::{self as fat, Fs, Timestamp};

pub const USAGE: &str = "\
usage:
  cerulean card new <img> [--size N] [--label L] [--from DIR]
                         空のカードのイメージを作る（既定 64M。8M〜512M。K/M 単位可）。
                         --from でホストのフォルダの中身を入れる
  cerulean card ls <img> [path]        一覧（path を省略するとルート）
  cerulean card put <img> <src>... [--to DIR]
                         ホストのファイル・フォルダ（中身ごと）をカードの DIR に入れる
  cerulean card get <img> <path> <dest> カードのファイル・フォルダをホストの dest に取り出す
  cerulean card rm <img> <path>        カードのファイル・フォルダ（中身ごと）を消す
  cerulean card mkdir <img> <path>     カードにフォルダを作る
";

pub fn cmd_card(args: &[String]) -> Result<ExitCode, String> {
    let Some((sub, rest)) = args.split_first() else {
        return Err(USAGE.into());
    };
    match sub.as_str() {
        "new" => card_new(rest),
        "ls" => card_ls(rest),
        "put" => card_put(rest),
        "get" => card_get(rest),
        "rm" => card_rm(rest),
        "mkdir" => card_mkdir(rest),
        _ => Err(USAGE.into()),
    }
    .map(|_| ExitCode::SUCCESS)
}

fn parse_size(s: &str) -> Result<u64, String> {
    let (num, mul) = match s.as_bytes().last() {
        Some(b'K' | b'k') => (&s[..s.len() - 1], 1u64 << 10),
        Some(b'M' | b'm') => (&s[..s.len() - 1], 1 << 20),
        Some(b'G' | b'g') => (&s[..s.len() - 1], 1 << 30),
        _ => (s, 1),
    };
    num.parse::<u64>()
        .ok()
        .and_then(|n| n.checked_mul(mul))
        .ok_or_else(|| format!("bad size {s:?}"))
}

/// ホストの時刻（SystemTime）を FAT の時刻に。
/// TODO: FAT の時刻はローカル時刻だが、タイムゾーンの情報を持たないので UTC で入れる
/// （--rtc の既定と同じ扱い）。
fn fat_time(t: SystemTime) -> Timestamp {
    let secs = t
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let days = secs.div_euclid(86400);
    let rem = secs.rem_euclid(86400);
    // 1970-01-01 からの日数を年月日に（グレゴリオ暦の民間の算法）
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + if month <= 2 { 1 } else { 0 };
    Timestamp {
        year: year.clamp(0, u16::MAX as i64) as u16,
        month: month as u8,
        day: day as u8,
        hour: (rem / 3600) as u8,
        minute: (rem / 60 % 60) as u8,
        second: (rem % 60) as u8,
    }
}

fn mtime(p: &Path) -> Timestamp {
    std::fs::metadata(p)
        .and_then(|m| m.modified())
        .map(fat_time)
        .unwrap_or_else(|_| fat_time(SystemTime::now()))
}

fn read_img(path: &str) -> Result<Vec<u8>, String> {
    std::fs::read(path).map_err(|e| format!("{path}: {e}"))
}

/// 書き込みは一時ファイル経由で置き換える（途中で失敗してもイメージを壊さない）。
fn write_img(path: &str, data: &[u8]) -> Result<(), String> {
    let tmp = format!("{path}.tmp");
    std::fs::write(&tmp, data).map_err(|e| format!("{tmp}: {e}"))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("{path}: {e}"))
}

fn open(img: &mut [u8]) -> Result<Fs<'_>, String> {
    Fs::open(img).map_err(|e| e.to_string())
}

/// ホストのファイル・フォルダを、カードの dest（パス）にそのまま入れる。
fn put_path(fs: &mut Fs, src: &Path, dest: &str) -> Result<(), String> {
    let meta = std::fs::metadata(src).map_err(|e| format!("{}: {e}", src.display()))?;
    if meta.is_dir() {
        fs.mkdir(dest, mtime(src)).map_err(|e| e.to_string())?;
        let mut entries: Vec<PathBuf> = std::fs::read_dir(src)
            .map_err(|e| format!("{}: {e}", src.display()))?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .collect();
        entries.sort(); // 反復の順を決める（同じフォルダから同じイメージを作る）
        for p in entries {
            let name = p
                .file_name()
                .and_then(|n| n.to_str())
                .ok_or_else(|| format!("{}: file name is not UTF-8", p.display()))?;
            put_path(fs, &p, &format!("{}/{name}", dest.trim_end_matches('/')))?;
        }
    } else {
        let data = std::fs::read(src).map_err(|e| format!("{}: {e}", src.display()))?;
        fs.write_file(dest, &data, mtime(src))
            .map_err(|e| format!("{}: {e}", src.display()))?;
    }
    Ok(())
}

fn card_new(args: &[String]) -> Result<(), String> {
    let mut it = args.iter();
    let mut img = None;
    let mut size = 64u64 << 20;
    let mut label = "STORAGECARD".to_string();
    let mut from = None;
    while let Some(a) = it.next() {
        let mut val = || {
            it.next()
                .cloned()
                .ok_or_else(|| format!("{a} needs a value"))
        };
        match a.as_str() {
            "--size" => size = parse_size(&val()?)?,
            "--label" => label = val()?,
            "--from" => from = Some(val()?),
            s if s.starts_with("--") => return Err(format!("unknown option {s}\n{USAGE}")),
            s => {
                if img.replace(s.to_string()).is_some() {
                    return Err(USAGE.into());
                }
            }
        }
    }
    let path = img.ok_or(USAGE)?;
    let mut d = fat::format(size, &label).map_err(|e| e.to_string())?;
    if let Some(dir) = from {
        let mut fs = open(&mut d)?;
        let entries = std::fs::read_dir(&dir).map_err(|e| format!("{dir}: {e}"))?;
        let mut paths: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
        paths.sort();
        for p in paths {
            let name = p
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default()
                .to_string();
            put_path(&mut fs, &p, &name)?;
        }
    }
    write_img(&path, &d)
}

fn card_ls(args: &[String]) -> Result<(), String> {
    let (img, path) = match args {
        [i] => (i, "/"),
        [i, p] => (i, p.as_str()),
        _ => return Err(USAGE.into()),
    };
    let mut d = read_img(img)?;
    let fs = open(&mut d)?;
    for e in fs.list(path).map_err(|e| e.to_string())? {
        let t = e.modified;
        let size = if e.is_dir {
            "<DIR>".to_string()
        } else {
            e.size.to_string()
        };
        println!(
            "{:04}-{:02}-{:02} {:02}:{:02}  {size:>10}  {}",
            t.year, t.month, t.day, t.hour, t.minute, e.name
        );
    }
    println!("free: {} bytes", fs.free_bytes());
    Ok(())
}

fn card_put(args: &[String]) -> Result<(), String> {
    let mut it = args.iter();
    let img = it.next().ok_or(USAGE)?;
    let mut srcs = vec![];
    let mut to = String::new();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--to" => to = it.next().cloned().ok_or("--to needs a value")?,
            s => srcs.push(s.to_string()),
        }
    }
    if srcs.is_empty() {
        return Err(USAGE.into());
    }
    let mut d = read_img(img)?;
    {
        let mut fs = open(&mut d)?;
        for s in &srcs {
            let p = Path::new(s);
            let name = p
                .file_name()
                .and_then(|n| n.to_str())
                .ok_or_else(|| format!("{s}: bad file name"))?;
            let dest = if to.is_empty() {
                name.to_string()
            } else {
                format!("{}/{name}", to.trim_end_matches('/'))
            };
            put_path(&mut fs, p, &dest)?;
        }
    }
    write_img(img, &d)
}

fn get_path(fs: &Fs, src: &str, dest: &Path) -> Result<(), String> {
    match fs.read_file(src) {
        Ok(data) => std::fs::write(dest, data).map_err(|e| format!("{}: {e}", dest.display())),
        Err(_) => {
            // フォルダとして取り出す（ルートも含む）
            let list = fs.list(src).map_err(|e| format!("{src}: {e}"))?;
            std::fs::create_dir_all(dest).map_err(|e| format!("{}: {e}", dest.display()))?;
            for e in list {
                let child = format!("{}/{}", src.trim_end_matches('/'), e.name);
                get_path(fs, &child, &dest.join(&e.name))?;
            }
            Ok(())
        }
    }
}

fn card_get(args: &[String]) -> Result<(), String> {
    let [img, path, dest] = args else {
        return Err(USAGE.into());
    };
    let mut d = read_img(img)?;
    let fs = open(&mut d)?;
    get_path(&fs, path, Path::new(dest))
}

fn card_rm(args: &[String]) -> Result<(), String> {
    let [img, path] = args else {
        return Err(USAGE.into());
    };
    let mut d = read_img(img)?;
    open(&mut d)?
        .remove(path, true)
        .map_err(|e| e.to_string())?;
    write_img(img, &d)
}

fn card_mkdir(args: &[String]) -> Result<(), String> {
    let [img, path] = args else {
        return Err(USAGE.into());
    };
    let mut d = read_img(img)?;
    open(&mut d)?
        .mkdir(path, fat_time(SystemTime::now()))
        .map_err(|e| e.to_string())?;
    write_img(img, &d)
}
