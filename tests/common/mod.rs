#![allow(dead_code)]
//! E2E/統合テスト共通: 常駐HTTPサーバ、E2Eゲート、fixture。

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

pub struct Route {
    pub path: &'static str,
    pub body: Vec<u8>,
    pub content_type: &'static str,
    pub headers: Vec<(&'static str, String)>,
    pub delay_ms: u64,
}

impl Route {
    pub fn html(path: &'static str, body: impl Into<String>) -> Self {
        Route {
            path,
            body: body.into().into_bytes(),
            content_type: "text/html; charset=utf-8",
            headers: vec![],
            delay_ms: 0,
        }
    }
}

pub struct Server {
    pub port: u16,
    stop: Arc<AtomicBool>,
}

impl Server {
    pub fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{}", self.port, path)
    }
}

/// テストごとにaccept loopを畳む。listenerスレッドをプロセス終了まで残さない。
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // accept待ちをほどくために自分へ1本つなぐ。
        let _ = TcpStream::connect(("127.0.0.1", self.port));
    }
}

/// 任意回数の要求に応答するサーバ。ルートはArcで共有し、接続ごとの複製をしない。
pub fn start(routes: Vec<Route>) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let stop = Arc::new(AtomicBool::new(false));
    let routes = Arc::new(routes);
    let stop_t = stop.clone();
    thread::spawn(move || {
        for stream in listener.incoming() {
            if stop_t.load(Ordering::SeqCst) {
                break;
            }
            let Ok(stream) = stream else { continue };
            let routes = routes.clone();
            thread::spawn(move || serve_one(stream, &routes));
        }
    });
    Server { port, stop }
}

fn serve_one(mut stream: TcpStream, routes: &[Route]) {
    let mut buf = [0u8; 8192];
    let n = stream.read(&mut buf).unwrap_or(0);
    let req = String::from_utf8_lossy(&buf[..n]);
    let path = req
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .unwrap_or("/")
        .to_string();
    let path_only = path.split('?').next().unwrap_or("/");
    match routes.iter().find(|r| r.path == path_only) {
        Some(r) => {
            let (body, ct, headers, delay) = (&r.body, r.content_type, &r.headers, r.delay_ms);
            if delay > 0 {
                thread::sleep(Duration::from_millis(delay));
            }
            let mut head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: {ct}\r\nContent-Length: {}\r\nConnection: close\r\n",
                body.len()
            );
            for (k, v) in headers {
                head.push_str(&format!("{k}: {v}\r\n"));
            }
            head.push_str("\r\n");
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(body);
        }
        None => {
            let _ = stream.write_all(
                b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            );
        }
    }
    let _ = stream.flush();
}

pub static E2E_LOCK: Mutex<()> = Mutex::new(());

/// WEBGRAB_E2E=1で有効。CI下で未設定なら失敗、それ以外は理由を出してskip。
pub fn e2e_enabled() -> bool {
    if std::env::var("WEBGRAB_E2E").as_deref() == Ok("1") {
        return true;
    }
    if std::env::var("CI").is_ok() {
        panic!("WEBGRAB_E2E must be set to 1 in CI (render E2E would be silently skipped)");
    }
    eprintln!("skip: set WEBGRAB_E2E=1 to run render E2E (requires Chrome)");
    false
}

fn env_args() -> Vec<String> {
    let mut v = Vec::new();
    if let Ok(p) = std::env::var("WEBGRAB_CHROME") {
        v.push("--chrome-path".into());
        v.push(p);
    }
    if std::env::var("WEBGRAB_E2E_NO_SANDBOX").as_deref() == Ok("1") {
        v.push("--no-sandbox".into());
    }
    v
}

fn run(args: Vec<String>) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_webgrab"))
        .args(&args)
        .output()
        .expect("binary runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

/// 環境変数由来のフラグ（--chrome-path / --no-sandbox）を付けて実行する。
pub fn webgrab(args: &[&str]) -> (i32, String, String) {
    let mut v: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    v.extend(env_args());
    run(v)
}

/// 環境変数を無視して実行する（E8用）。
pub fn webgrab_raw(args: &[&str]) -> (i32, String, String) {
    run(args.iter().map(|s| s.to_string()).collect())
}

// htmdはMarkdownの強調記号と衝突しないよう本文中の`_`を`\_`にエスケープする。
// 番兵はstdout/JSONの`markdown`フィールドとcontainsで突き合わせるため、
// エスケープされないハイフン区切りにする（SENTINEL_GZIP/SENTINEL_DOMはmarkdown化を経ずに
// exit codeのみで判定するためアンダースコアのまま、fixture生成コマンドとREADMEの文字列と一致させる）。
pub const SENTINEL_FAST: &str = "SENTINEL-FAST-1a2b";
pub const SENTINEL_SLOW: &str = "SENTINEL-SLOW-3c4d";
pub const SENTINEL_XHR: &str = "SENTINEL-XHR-5e6f";
pub const SENTINEL_STATIC: &str = "SENTINEL-STATIC-7a8b";
pub const SENTINEL_SHORT: &str = "SENTINEL-SHORT-9c0d";
pub const SENTINEL_GZIP: &str = "SENTINEL_GZIP_9f3c";
pub const SENTINEL_DOM: &str = "SENTINEL_DOM_2e4f";

fn article(sentinel: &str) -> String {
    let para = "これはJavaScriptで後から挿入された本文です。抽出アルゴリズムが本文と認識できる十分な長さの日本語文章を用意しています。さらに文章を続けて厚みを持たせます。";
    format!(
        "<article><h1>記事 {sentinel}</h1><p>{para}</p><p>{para}</p><p>{para}</p><p>{para}</p></article>"
    )
}

fn csr(path: &'static str, delay_ms: u64, placeholder: &str, sentinel: &str) -> Route {
    let art = article(sentinel).replace('\'', "\\'");
    Route::html(
        path,
        format!(
            "<html><head><meta charset=\"utf-8\"><title>CSR</title></head><body><div id=\"app\">{placeholder}</div>\
         <script>setTimeout(function(){{document.getElementById('app').innerHTML='{art}';}},{delay_ms});</script></body></html>"
        ),
    )
}

pub fn csr_fast() -> Route {
    csr("/csr_fast", 500, "", SENTINEL_FAST)
}
pub fn csr_slow() -> Route {
    csr("/csr_slow", 2500, "読み込み中...", SENTINEL_SLOW)
}

pub fn csr_xhr() -> Vec<Route> {
    let page = Route::html(
        "/csr_xhr",
        "<html><head><meta charset=\"utf-8\"><title>XHR</title></head><body><div id=\"app\"></div>\
        <script>fetch('/api/data').then(function(r){return r.text()}).then(function(t){document.getElementById('app').innerHTML=t;});</script></body></html>",
    );
    let api = Route {
        path: "/api/data",
        body: article(SENTINEL_XHR).into_bytes(),
        content_type: "text/html; charset=utf-8",
        headers: vec![],
        delay_ms: 1000,
    };
    vec![page, api]
}

pub fn static_article() -> Route {
    Route::html(
        "/static",
        format!(
            "<html><head><meta charset=\"utf-8\"><title>static</title></head><body>{}</body></html>",
            article(SENTINEL_STATIC)
        ),
    )
}

/// 静的150文字前後の本文。JSは20文字のシェルに置換する（JSチャレンジ模擬）。
pub fn short_static() -> Route {
    Route::html(
        "/short",
        format!(
            "<html><head><meta charset=\"utf-8\"><title>short</title></head><body><article id=\"a\"><p>これは百五十文字程度の短い本文です {SENTINEL_SHORT}。抽出器が本文として認識できる長さはありますが二百文字には届きません。エスカレーション判定の境界を確認するための固定文です。末尾。</p></article>\
         <script>document.getElementById('a').innerHTML='<p>Please enable JS.</p>';</script></body></html>"
        ),
    )
}

pub fn big_gzip() -> Route {
    Route {
        path: "/big_gzip",
        body: include_bytes!("../fixtures/big_gzip.html.gz").to_vec(),
        content_type: "text/html; charset=utf-8",
        headers: vec![("Content-Encoding", "gzip".to_string())],
        delay_ms: 0,
    }
}

/// 1KiBの文書で、JSがネットワークを経ずに3MiBのDOMを作る。
pub fn dom_bomb() -> Route {
    Route::html(
        "/dom_bomb",
        format!(
            "<html><head><meta charset=\"utf-8\"><title>dom</title></head><body><div id=\"app\">{SENTINEL_DOM}</div>\
         <script>var s='<p>'+'y'.repeat(1048576)+'</p>';document.getElementById('app').innerHTML=s+s+s;</script></body></html>"
        ),
    )
}

#[cfg(test)]
mod server_tests {
    use super::*;
    use std::time::Instant;

    fn get(port: u16, path: &str) -> String {
        let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
        c.write_all(
            format!("GET {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n").as_bytes(),
        )
        .unwrap();
        let mut buf = Vec::new();
        c.read_to_end(&mut buf).unwrap();
        String::from_utf8_lossy(&buf).to_string()
    }

    #[test]
    fn shared_routes_serve_repeated_requests() {
        let s = start(vec![
            Route::html("/a", "<p>alpha</p>"),
            Route::html("/b", "<p>beta</p>"),
        ]);
        assert!(get(s.port, "/a").contains("alpha"));
        assert!(get(s.port, "/b").contains("beta"));
        assert!(get(s.port, "/a?q=1").contains("alpha"));
        assert!(get(s.port, "/missing").contains("404"));
    }

    #[test]
    fn dropping_server_stops_accept_loop() {
        let s = start(vec![Route::html("/a", "<p>alpha</p>")]);
        let port = s.port;
        assert!(get(port, "/a").contains("alpha"));
        drop(s);
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if TcpStream::connect(("127.0.0.1", port)).is_err() {
                return;
            }
            assert!(Instant::now() < deadline, "accept loopが止まっていない");
            thread::sleep(Duration::from_millis(20));
        }
    }
}
