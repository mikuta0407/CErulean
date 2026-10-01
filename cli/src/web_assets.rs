//! ビルド時に埋め込んだ静的ファイル。配信と静的サイトの書き出しで同じ内容を使う。
use std::path::Path;

#[cfg(feature = "embedded-web")]
include!(concat!(env!("OUT_DIR"), "/web_assets.rs"));
#[cfg(not(feature = "embedded-web"))]
pub const FILES: &[(&str, &[u8])] = &[];

pub fn get(path: &str) -> Option<&'static [u8]> {
    FILES
        .iter()
        .find(|(name, _)| *name == path)
        .map(|(_, b)| *b)
}

pub fn cmd_export(args: &[String]) -> Result<std::process::ExitCode, String> {
    let [dir] = args else {
        return Err("usage: cerulean web-export <new-directory>".into());
    };
    if FILES.is_empty() {
        return Err("Web assets are not embedded; build with tools/build.sh".into());
    }
    let root = Path::new(dir);
    // 既存のサイトや利用者のファイルを上書きしない。
    if let Some(parent) = root.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    std::fs::create_dir(root)
        .map_err(|e| format!("{}: {e} (use a new directory)", root.display()))?;
    for &(name, bytes) in FILES {
        let path = root.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        std::fs::write(&path, bytes).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    eprintln!(
        "cerulean web-export: {} (open app/ over HTTPS or localhost)",
        root.display()
    );
    Ok(std::process::ExitCode::SUCCESS)
}

#[cfg(all(test, feature = "embedded-web"))]
mod tests {
    use super::*;

    #[test]
    fn export_matches_embedded_assets_and_refuses_overwrite() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "cerulean-web-export-{}-{nonce}",
            std::process::id()
        ));
        let args = [dir.to_str().unwrap().to_string()];
        cmd_export(&args).unwrap();
        for &(name, bytes) in FILES {
            assert_eq!(std::fs::read(dir.join(name)).unwrap(), bytes);
        }
        std::fs::write(dir.join("index.html"), b"keep me").unwrap();
        assert!(cmd_export(&args).is_err());
        assert_eq!(std::fs::read(dir.join("index.html")).unwrap(), b"keep me");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
