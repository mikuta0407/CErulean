//! `cerulean serve`: 埋め込んだブラウザ版を配信する小さな Web サーバー。
//! `--with-relay` で同じポートの /relay に中継サーバー（relay.rs）も置く。
//!
//! 配信するのは静的なファイルだけ（GET・HEAD）。ブラウザが古い worker.js や wasm を使い
//! 回さないよう Cache-Control: no-store を付ける（tools/serve-bench.py と同じ理由）。
//! TLS は付けない（公開するときは前段のリバースプロキシで付ける。Web Worker の OPFS・
//! SubtleCrypto は安全なコンテキスト（HTTPS か localhost）でないと使えない）。

use std::io::Write;
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::relay::{self, RelayOpts};
use crate::web_assets;

const USAGE: &str = "usage: cerulean serve [--listen ADDR:PORT | --port N] [--root DIR]
                      [--with-relay [--token T] [--allow-private]]
  --listen A       待ち受けるアドレス（既定 127.0.0.1:8000）
  --port N         待ち受けるポート（アドレスは 127.0.0.1。他の端末から使うときは --listen 0.0.0.0:N）
  --root DIR       外部の Web 資材を配信する（app/ と pkg/ を含む。既定は埋め込み資材）
  --with-relay     /relay でネットワークの中継サーバーも動かす（WebSocket）
  --token T        中継のトークン（省くと起動時に乱数で作って表示する）
  --allow-private  中継で私的アドレス（LAN・localhost 等）への接続も許す（既定は断る）";

struct Site {
    root: Option<PathBuf>,
    relay: Option<RelayOpts>,
}

pub fn cmd_serve(args: &[String]) -> Result<std::process::ExitCode, String> {
    let mut listen = "127.0.0.1:8000".to_string();
    let mut root = None;
    let (mut with_relay, mut token, mut allow_private) = (false, None, false);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut val = || {
            it.next()
                .cloned()
                .ok_or_else(|| format!("{a} needs a value"))
        };
        match a.as_str() {
            "--listen" => listen = val()?,
            "--port" => listen = format!("127.0.0.1:{}", val()?),
            "--root" => root = Some(PathBuf::from(val()?)),
            "--with-relay" => with_relay = true,
            "--token" => token = Some(val()?),
            "--allow-private" => allow_private = true,
            _ => return Err(USAGE.into()),
        }
    }
    if !with_relay && (token.is_some() || allow_private) {
        return Err("--token and --allow-private need --with-relay".into());
    }
    let root = match root {
        Some(r) => Some(r),
        None if !web_assets::FILES.is_empty() => None,
        None => Some(find_root()?),
    };
    if let Some(root) = &root {
        if !root.join("app/index.html").is_file() {
            return Err(format!("{}: app/index.html not found", root.display()));
        }
        if !root.join("pkg/cerulean_web_bg.wasm").is_file() {
            return Err(format!(
                "{}: pkg/cerulean_web_bg.wasm not found (build it with tools/web-build.sh)",
                root.display()
            ));
        }
    }
    let relay = if with_relay {
        Some(RelayOpts {
            token: relay::token_or_random(token)?,
            allow_private,
        })
    } else {
        None
    };
    let l = TcpListener::bind(&listen).map_err(|e| format!("{listen}: {e}"))?;
    let site = Arc::new(Site { root, relay });
    eprintln!(
        "cerulean serve: {} on http://{listen}/",
        site.root.as_ref().map_or_else(
            || "embedded Web assets".to_string(),
            |r| r.display().to_string()
        )
    );
    match &site.relay {
        Some(r) => {
            eprintln!(
                "cerulean serve: open http://{listen}/app/#relay-token={}",
                r.token
            );
            eprintln!(
                "cerulean serve: relay on ws://{listen}/relay (token {}{})",
                r.token,
                if r.allow_private {
                    ", private addresses allowed"
                } else {
                    ""
                }
            );
        }
        None => eprintln!("cerulean serve: open http://{listen}/app/"),
    }
    for s in l.incoming() {
        let Ok(s) = s else { continue };
        let site = site.clone();
        std::thread::spawn(move || {
            let peer = s.peer_addr().map(|a| a.to_string()).unwrap_or_default();
            if let Err(e) = handle(s, &site) {
                eprintln!("cerulean serve: {peer}: {e}");
            }
        });
    }
    Ok(std::process::ExitCode::SUCCESS)
}

/// 既定の配信元: 今のディレクトリか実行ファイルの場所から web/www を探す。
fn find_root() -> Result<PathBuf, String> {
    let mut starts = vec![std::env::current_dir().map_err(|e| e.to_string())?];
    if let Ok(exe) = std::env::current_exe() {
        starts.push(exe);
    }
    for s in starts {
        for dir in s.ancestors() {
            let p = dir.join("web/www");
            if p.join("app/index.html").is_file() {
                return Ok(p);
            }
        }
    }
    Err("Web assets are not embedded; build with tools/build.sh or use --root DIR".into())
}

fn handle(mut s: TcpStream, site: &Site) -> Result<(), String> {
    // 1 接続 1 要求（Connection: close）
    let head = relay::read_head(&mut s)?;
    let line = head.lines().next().unwrap_or("");
    let mut parts = line.split(' ');
    let (method, target) = (parts.next().unwrap_or(""), parts.next().unwrap_or("/"));
    let path = target.split(['?', '#']).next().unwrap_or("/");
    if path == "/relay" || path.starts_with("/relay/") {
        return match &site.relay {
            Some(r) if relay::is_websocket(&head) => relay::serve_ws(s, &head, r),
            Some(_) => respond(&mut s, 400, "text/plain", b"WebSocket only here\n", method),
            None => respond(
                &mut s,
                404,
                "text/plain",
                b"relay is off (--with-relay)\n",
                method,
            ),
        };
    }
    if method != "GET" && method != "HEAD" {
        return respond(&mut s, 405, "text/plain", b"method not allowed\n", method);
    }
    if path == "/" {
        return redirect(&mut s, "/app/");
    }
    let Some(root) = &site.root else {
        return serve_embedded(&mut s, path, method);
    };
    let Some(file) = resolve(root, path) else {
        return respond(&mut s, 404, "text/plain", b"not found\n", method);
    };
    if file.is_dir() {
        if !path.ends_with('/') {
            return redirect(&mut s, &format!("{path}/"));
        }
        let index = file.join("index.html");
        return serve_file(&mut s, &index, method);
    }
    serve_file(&mut s, &file, method)
}

/// 埋め込みファイルも外部ファイルも同じパス検査を使う。
fn asset_name(path: &str) -> Option<String> {
    let decoded = percent_decode(path)?;
    if decoded.contains(['\\', '\0']) {
        return None;
    }
    let mut parts = Vec::new();
    for part in decoded.split('/') {
        match part {
            "" | "." => {}
            ".." => return None,
            p if p.contains(':') => return None,
            p => parts.push(p),
        }
    }
    Some(parts.join("/"))
}

fn serve_embedded(s: &mut TcpStream, path: &str, method: &str) -> Result<(), String> {
    let Some(name) = asset_name(path) else {
        return respond(s, 404, "text/plain", b"not found\n", method);
    };
    if let Some(body) = web_assets::get(&name) {
        return respond(s, 200, mime(Path::new(&name)), body, method);
    }
    let index = format!("{name}/index.html");
    if let Some(body) = web_assets::get(&index) {
        if !path.ends_with('/') {
            return redirect(s, &format!("{path}/"));
        }
        return respond(s, 200, mime(Path::new(&index)), body, method);
    }
    respond(s, 404, "text/plain", b"not found\n", method)
}

/// URL のパス（%xx を戻す）を配信元の中のファイルにする。配信元の外（..）は None。
fn resolve(root: &Path, path: &str) -> Option<PathBuf> {
    let p = root.join(asset_name(path)?);
    p.exists().then_some(p)
}

fn percent_decode(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let h = std::str::from_utf8(b.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(h, 16).ok()?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

fn mime(p: &Path) -> &'static str {
    match p.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "wasm" => "application/wasm",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json",
        "webmanifest" => "application/manifest+json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "txt" | "md" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

fn serve_file(s: &mut TcpStream, p: &Path, method: &str) -> Result<(), String> {
    match std::fs::read(p) {
        Ok(body) => respond(s, 200, mime(p), &body, method),
        Err(_) => respond(s, 404, "text/plain", b"not found\n", method),
    }
}

fn redirect(s: &mut TcpStream, to: &str) -> Result<(), String> {
    let h = format!(
        "HTTP/1.1 302 Found\r\nLocation: {to}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    );
    s.write_all(h.as_bytes()).map_err(|e| e.to_string())
}

fn respond(
    s: &mut TcpStream,
    code: u16,
    ctype: &str,
    body: &[u8],
    method: &str,
) -> Result<(), String> {
    let reason = match code {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        405 => "Method Not Allowed",
        _ => "Error",
    };
    let h = format!(
        "HTTP/1.1 {code} {reason}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\n\r\n",
        body.len()
    );
    s.write_all(h.as_bytes()).map_err(|e| e.to_string())?;
    if method != "HEAD" {
        s.write_all(body).map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_stays_inside_root() {
        let root = std::env::temp_dir();
        assert!(resolve(&root, "/../etc/passwd").is_none());
        assert!(resolve(&root, "/%2e%2e/etc/passwd").is_none());
        assert_eq!(percent_decode("/a%20b").as_deref(), Some("/a b"));
    }

    #[cfg(feature = "embedded-web")]
    fn request(method: &str, path: &str) -> Vec<u8> {
        use std::io::Read;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            handle(
                stream,
                &Site {
                    root: None,
                    relay: None,
                },
            )
            .unwrap();
        });
        let mut stream = TcpStream::connect(addr).unwrap();
        stream
            .write_all(format!("{method} {path} HTTP/1.1\r\nHost: localhost\r\n\r\n").as_bytes())
            .unwrap();
        stream.shutdown(std::net::Shutdown::Write).unwrap();
        let mut response = Vec::new();
        stream.read_to_end(&mut response).unwrap();
        server.join().unwrap();
        response
    }

    #[cfg(feature = "embedded-web")]
    #[test]
    fn embedded_http() {
        let response = request("GET", "/app/");
        let split = response.windows(4).position(|b| b == b"\r\n\r\n").unwrap() + 4;
        assert!(response.starts_with(b"HTTP/1.1 200"));
        assert_eq!(
            &response[split..],
            web_assets::get("app/index.html").unwrap()
        );
        for (method, path, status) in [
            ("GET", "/", "302"),
            ("GET", "/app", "302"),
            ("GET", "/app/worker.js?version=test", "200"),
            ("GET", "/%2e%2e/Cargo.toml", "404"),
            ("GET", "/app/%2e%2e/pkg/cerulean_web.js", "404"),
            ("GET", "/no-such-file", "404"),
            ("GET", "/relay", "404"),
            ("POST", "/app/", "405"),
        ] {
            assert!(
                request(method, path).starts_with(format!("HTTP/1.1 {status}").as_bytes()),
                "{method} {path}"
            );
        }
        let response = request("HEAD", "/pkg/cerulean_web_bg.wasm");
        let head = std::str::from_utf8(&response).unwrap();
        assert!(head.starts_with("HTTP/1.1 200"));
        assert!(head.contains("Content-Type: application/wasm\r\n"));
        assert!(head.contains(&format!(
            "Content-Length: {}\r\n",
            web_assets::get("pkg/cerulean_web_bg.wasm").unwrap().len()
        )));
        assert!(head.ends_with("\r\n\r\n"));
    }
}
