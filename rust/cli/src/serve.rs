//! `cerulean serve`: ブラウザ版（rust/web/www）を配信する小さな Web サーバー。
//! `--with-relay` で同じポートの /relay に中継サーバー（relay.rs）も置く。
//!
//! 配信するのは静的なファイルだけ（GET・HEAD）。ブラウザが古い worker.js や wasm を使い
//! 回さないよう Cache-Control: no-store を付ける（tools/serve-bench.py と同じ理由）。
//! TLS は付けない（公開するときは前段のリバースプロキシで付ける。Web Worker の OPFS・
//! SubtleCrypto は安全なコンテキスト（HTTPS か localhost）でないと使えない）。

use std::io::Write;
use std::net::{TcpListener, TcpStream};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use crate::relay::{self, RelayOpts};

const USAGE: &str = "usage: cerulean serve [--listen ADDR:PORT | --port N] [--root DIR]
                      [--with-relay [--token T] [--allow-private]]
  --listen A       待ち受けるアドレス（既定 127.0.0.1:8000）
  --port N         待ち受けるポート（アドレスは 127.0.0.1。他の端末から使うときは --listen 0.0.0.0:N）
  --root DIR       配信する rust/web/www（app/ と pkg/ を含む。既定は探す）
  --with-relay     /relay でネットワークの中継サーバーも動かす（WebSocket）
  --token T        中継のトークン（省くと起動時に乱数で作って表示する）
  --allow-private  中継で私的アドレス（LAN・localhost 等）への接続も許す（既定は断る）";

struct Site {
    root: PathBuf,
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
        Some(r) => r,
        None => find_root()?,
    };
    if !root.join("app/index.html").is_file() {
        return Err(format!("{}: app/index.html not found", root.display()));
    }
    if !root.join("pkg/cerulean_web_bg.wasm").is_file() {
        return Err(format!(
            "{}: pkg/cerulean_web_bg.wasm not found (build it with tools/web-build.sh)",
            root.display()
        ));
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
        site.root.display()
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

/// 既定の配信元: 今のディレクトリか実行ファイルの場所から rust/web/www を探す。
fn find_root() -> Result<PathBuf, String> {
    let mut starts = vec![std::env::current_dir().map_err(|e| e.to_string())?];
    if let Ok(exe) = std::env::current_exe() {
        starts.push(exe);
    }
    for s in starts {
        for dir in s.ancestors() {
            for cand in ["rust/web/www", "web/www"] {
                let p = dir.join(cand);
                if p.join("app/index.html").is_file() {
                    return Ok(p);
                }
            }
        }
    }
    Err("rust/web/www not found (use --root)".into())
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
    let Some(file) = resolve(&site.root, path) else {
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

/// URL のパス（%xx を戻す）を配信元の中のファイルにする。配信元の外（..）は None。
fn resolve(root: &Path, path: &str) -> Option<PathBuf> {
    let decoded = percent_decode(path)?;
    let mut p = root.to_path_buf();
    for c in Path::new(decoded.trim_start_matches('/')).components() {
        match c {
            Component::Normal(x) => p.push(x),
            Component::CurDir => {}
            _ => return None,
        }
    }
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
}
