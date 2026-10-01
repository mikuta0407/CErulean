//! フォルダ共有（Device Emulator の「Storage Card」）の中身とカードのイメージ（FAT）の変換
//! （CLI の cli/src/share.rs と同じもの）。
//! 属性は FAT 側がディレクトリかどうかしか扱わないので、読み取り専用・隠し等は保たない。

use cerulean_core::smdk2410::deshare::{ShareEntry, ShareFs, dos_time};
use cerulean_fat::{Fs, Timestamp};

fn to_dos(t: Timestamp) -> u32 {
    dos_time(
        t.year as i64,
        t.month as i64,
        t.day as i64,
        t.hour as i64,
        t.minute as i64,
        t.second as i64,
    )
}

fn from_dos(v: u32) -> Timestamp {
    let (d, t) = ((v >> 16) as u16, v as u16);
    Timestamp {
        year: 1980 + (d >> 9),
        month: ((d >> 5) & 0xF) as u8,
        day: (d & 0x1F) as u8,
        hour: (t >> 11) as u8,
        minute: ((t >> 5) & 0x3F) as u8,
        second: ((t & 0x1F) * 2) as u8,
    }
}

/// カードのイメージの中身を共有フォルダにする。
pub fn from_image(img: &[u8]) -> Result<ShareFs, String> {
    let mut d = img.to_vec();
    let fs = Fs::open(&mut d).map_err(|e| e.to_string())?;
    let mut out = ShareFs::new();
    let mut stack = vec![String::new()];
    while let Some(dir) = stack.pop() {
        for e in fs.list(&dir).map_err(|e| e.to_string())? {
            let p = if dir.is_empty() {
                e.name.clone()
            } else {
                format!("{dir}/{}", e.name)
            };
            let data = if e.is_dir {
                Vec::new()
            } else {
                fs.read_file(&p).map_err(|e| e.to_string())?
            };
            let t = to_dos(e.modified);
            out.insert(
                &format!("\\{}", p.replace('/', "\\")),
                ShareEntry {
                    name: e.name.clone(),
                    dir: e.is_dir,
                    attrs: if e.is_dir { 0x10 } else { 0x20 },
                    mtime: t,
                    ctime: t,
                    data,
                },
            )?;
            if e.is_dir {
                stack.push(p);
            }
        }
    }
    Ok(out)
}

/// 共有フォルダの中身をカードのイメージ（size バイトの FAT16）にする。
pub fn to_image(fs: &ShareFs, size: u64) -> Result<Vec<u8>, String> {
    let need = fs.used_bytes() + fs.used_bytes() / 8 + (4 << 20);
    let size = size.max(need.next_multiple_of(1 << 20)).clamp(
        cerulean_fat::MIN_FORMAT_BYTES,
        cerulean_fat::MAX_FORMAT_BYTES,
    );
    let mut img = cerulean_fat::format(size, "STORAGECARD").map_err(|e| e.to_string())?;
    let mut f = Fs::open(&mut img).map_err(|e| e.to_string())?;
    for (path, e) in fs.entries() {
        let p = path.trim_start_matches('\\').replace('\\', "/");
        let t = from_dos(e.mtime);
        if e.dir {
            f.mkdir(&p, t).map_err(|x| format!("{p}: {x}"))?;
        } else {
            f.write_file(&p, &e.data, t)
                .map_err(|x| format!("{p}: {x}"))?;
        }
    }
    Ok(img)
}
