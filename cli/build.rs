// Web 資材の埋め込みは配布用ビルドだけで行う。CLI / コアだけの開発に wasm の生成を要求しない。
use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if env::var_os("CARGO_FEATURE_EMBEDDED_WEB").is_none() {
        return;
    }
    let site = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap())
        .join("../web/www")
        .canonicalize()
        .expect("web/www not found");
    // 配布するファイルを列挙する。作業用イメージや不要な生成物を混入させない。
    let mut files: Vec<String> = [
        "index.html",
        "app/index.html",
        "app/app.js",
        "app/worker.js",
        "app/style.css",
        "app/sw.js",
        "app/manifest.webmanifest",
        "app/icon.svg",
        "app/icon-192.png",
        "app/icon-512.png",
        "pkg/cerulean_web.js",
        "pkg/cerulean_web_bg.wasm",
        "pkg/assets.json",
    ]
    .into_iter()
    .map(str::to_string)
    .collect();
    collect_snippets(&site, &site.join("pkg/snippets"), &mut files);
    files.sort();
    let mut generated = String::from("pub const FILES: &[(&str, &[u8])] = &[\n");
    for name in files {
        let path = site.join(&name);
        println!("cargo:rerun-if-changed={}", path.display());
        assert!(
            path.is_file(),
            "{} not found; run tools/web-build.sh before building with --features embedded-web (or use tools/build.sh)",
            path.display()
        );
        generated.push_str(&format!(
            "    ({name:?}, include_bytes!({:?})),\n",
            path.to_str().expect("asset path must be UTF-8")
        ));
    }
    generated.push_str("];\n");
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("web_assets.rs");
    fs::write(out, generated).expect("write web_assets.rs");
}

// wasm-bindgen の inline_js の出力先にはハッシュが付くため、名前を固定しない。
fn collect_snippets(site: &std::path::Path, dir: &std::path::Path, files: &mut Vec<String>) {
    println!("cargo:rerun-if-changed={}", dir.display());
    if !dir.exists() {
        return;
    }
    for entry in fs::read_dir(dir).expect("read wasm-bindgen snippets") {
        let entry = entry.expect("read snippet entry");
        let path = entry.path();
        let kind = entry.file_type().expect("snippet file type");
        if kind.is_dir() {
            collect_snippets(site, &path, files);
        } else if kind.is_file() && path.extension().is_some_and(|ext| ext == "js") {
            files.push(
                path.strip_prefix(site)
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .replace('\\', "/"),
            );
        }
    }
}
