//! Tiny localhost HTTP endpoint so a browser extension (or a bookmarklet, or
//! `curl`) can hand links to the running app.
//!
//!   GET  /ping                  -> {"app":"rdm"}
//!   POST /add   {"url": "...", "filename": "..."}
//!   POST /add   {"urls": ["...", "..."]}

use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use crate::engine::Engine;

pub fn spawn(engine: Arc<Engine>, port: u16) {
    let handle = engine.runtime();
    handle.spawn(async move {
        let listener = match TcpListener::bind(("127.0.0.1", port)).await {
            Ok(l) => l,
            Err(err) => {
                eprintln!("browser integration disabled: {err}");
                return;
            }
        };
        loop {
            let Ok((mut socket, _)) = listener.accept().await else { continue };
            let engine = engine.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 64 * 1024];
                let Ok(n) = socket.read(&mut buf).await else { return };
                let request = String::from_utf8_lossy(&buf[..n]).to_string();
                let response = handle_request(&engine, &request);
                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.shutdown().await;
            });
        }
    });
}

fn handle_request(engine: &Arc<Engine>, request: &str) -> String {
    let mut lines = request.split("\r\n");
    let start = lines.next().unwrap_or_default();
    let mut parts = start.split_whitespace();
    let method = parts.next().unwrap_or("");
    let path = parts.next().unwrap_or("");
    let body = request.split("\r\n\r\n").nth(1).unwrap_or("");

    match (method, path) {
        ("OPTIONS", _) => reply(204, ""),
        ("GET", "/ping") => reply(200, r#"{"app":"rdm","ok":true}"#),
        ("POST", "/add") => {
            let value: serde_json::Value = match serde_json::from_str(body) {
                Ok(v) => v,
                Err(err) => return reply(400, &format!(r#"{{"error":"{err}"}}"#)),
            };
            let filename = value.get("filename").and_then(|v| v.as_str()).map(str::to_string);
            // Carry the page the link came from, so protected files still download.
            let net = value.get("referer").and_then(|v| v.as_str()).map(|referer| {
                let mut net = engine.settings.lock().unwrap().net.clone();
                net.referer = Some(referer.to_string());
                if let Some(cookie) = value.get("cookie").and_then(|v| v.as_str()) {
                    net.cookie = Some(cookie.to_string());
                }
                net
            });
            let mut added = 0;
            if let Some(url) = value.get("url").and_then(|v| v.as_str()) {
                engine.add_full(url, filename.clone(), None, net.clone(), 0);
                added += 1;
            }
            if let Some(urls) = value.get("urls").and_then(|v| v.as_array()) {
                for url in urls.iter().filter_map(|u| u.as_str()) {
                    engine.add_full(url, None, None, net.clone(), 0);
                    added += 1;
                }
            }
            reply(200, &format!(r#"{{"added":{added}}}"#))
        }
        _ => reply(404, r#"{"error":"not found"}"#),
    }
}

fn reply(status: u16, body: &str) -> String {
    let text = match status {
        200 => "OK",
        204 => "No Content",
        400 => "Bad Request",
        _ => "Not Found",
    };
    format!(
        "HTTP/1.1 {status} {text}\r\n\
         Content-Type: application/json\r\n\
         Access-Control-Allow-Origin: *\r\n\
         Access-Control-Allow-Headers: *\r\n\
         Access-Control-Allow-Methods: GET, POST, OPTIONS\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\r\n{body}",
        body.len()
    )
}
