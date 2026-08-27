//! 統合テスト（設計§8）。ローカルTcpListenerでHTTPを模し、CLIバイナリを起動して
//! 終了コード・stdout/stderr分離・出力形式を検証する。

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::Command;
use std::thread;

/// 最小HTTPサーバ。1リクエストに1レスポンスを返し、指定回数で終了する。
/// `responder`はパスを受け取り、生のHTTPレスポンス文字列（ヘッダ+ボディ）を返す。
fn spawn_server<F>(count: usize, responder: F) -> u16
where
    F: Fn(&str) -> String + Send + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    thread::spawn(move || {
        for _ in 0..count {
            if let Ok((mut stream, _)) = listener.accept() {
                handle(&mut stream, &responder);
            }
        }
    });
    port
}

fn handle<F: Fn(&str) -> String>(stream: &mut TcpStream, responder: &F) {
    let mut buf = [0u8; 4096];
    let n = stream.read(&mut buf).unwrap_or(0);
    let req = String::from_utf8_lossy(&buf[..n]);
    let path = req
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .unwrap_or("/");
    let resp = responder(path);
    let _ = stream.write_all(resp.as_bytes());
    let _ = stream.flush();
}

fn http_response(body: &str, content_type: &str) -> String {
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        content_type,
        body.len(),
        body
    )
}

fn run_webgrab(args: &[&str]) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_webgrab"))
        .args(args)
        .output()
        .expect("binary runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

const ARTICLE: &str = "<html><head><title>統合テスト記事</title></head><body><article><h1>統合テスト記事</h1><p>これは統合テスト用の本文です。抽出アルゴリズムが本文と認識するために十分な長さの日本語文章を用意しています。さらに文章を続けて厚みを持たせます。</p></article></body></html>";

#[test]
fn success_returns_markdown_with_headers_and_exit_0() {
    let port = spawn_server(3, |path| {
        if path == "/robots.txt" {
            return "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                .into();
        }
        http_response(ARTICLE, "text/html; charset=utf-8")
    });
    let url = format!("http://127.0.0.1:{port}/article");
    let (code, stdout, _stderr) = run_webgrab(&[&url, "--allow-private"]);
    assert_eq!(code, 0, "stdout={stdout}");
    assert!(stdout.contains("Title:"));
    assert!(stdout.contains("Markdown Content:"));
    assert!(stdout.contains("統合テスト"));
}

#[test]
fn http_404_returns_exit_4() {
    let port = spawn_server(3, |path| {
        if path == "/robots.txt" {
            return "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                .into();
        }
        "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into()
    });
    let url = format!("http://127.0.0.1:{port}/missing");
    let (code, _stdout, stderr) = run_webgrab(&[&url, "--allow-private"]);
    assert_eq!(code, 4);
    assert!(stderr.contains("error=http"));
}

#[test]
fn robots_disallow_returns_exit_5() {
    let port = spawn_server(2, |path| {
        if path == "/robots.txt" {
            let body = "User-agent: *\nDisallow: /";
            return http_response(body, "text/plain");
        }
        http_response(ARTICLE, "text/html")
    });
    let url = format!("http://127.0.0.1:{port}/blocked");
    let (code, _stdout, stderr) = run_webgrab(&[&url, "--allow-private"]);
    assert_eq!(code, 5);
    assert!(stderr.contains("error=robots"));
}

#[test]
fn internal_address_without_flag_returns_exit_8() {
    // --allow-private を付けないので 127.0.0.1 は拒否される
    let port = spawn_server(1, |_| http_response(ARTICLE, "text/html"));
    let url = format!("http://127.0.0.1:{port}/x");
    let (code, _stdout, stderr) = run_webgrab(&[&url]);
    assert_eq!(code, 8);
    assert!(stderr.contains("error=netguard"));
}

#[test]
fn invalid_scheme_returns_exit_2() {
    let (code, _stdout, stderr) = run_webgrab(&["ftp://example.com/x"]);
    assert_eq!(code, 2);
    assert!(stderr.contains("error=usage"));
}

#[test]
fn json_format_emits_valid_json() {
    let port = spawn_server(3, |path| {
        if path == "/robots.txt" {
            return "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                .into();
        }
        http_response(ARTICLE, "text/html; charset=utf-8")
    });
    let url = format!("http://127.0.0.1:{port}/article");
    let (code, stdout, _stderr) = run_webgrab(&[&url, "--allow-private", "--format", "json"]);
    assert_eq!(code, 0);
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect("valid json");
    assert!(v["markdown"].as_str().unwrap().contains("統合テスト"));
    assert_eq!(v["truncated"], false);
}

#[test]
fn pagination_truncates_and_emits_continue_footer() {
    let port = spawn_server(3, |path| {
        if path == "/robots.txt" {
            return "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                .into();
        }
        http_response(ARTICLE, "text/html; charset=utf-8")
    });
    let url = format!("http://127.0.0.1:{port}/article");
    let (code, stdout, _stderr) = run_webgrab(&[&url, "--allow-private", "--max-chars", "10"]);
    assert_eq!(code, 0);
    assert!(stdout.contains("[webgrab:truncated"));
    assert!(stdout.contains("--start-index 10"));
}

#[test]
fn robots_redirect_is_manually_followed_once() {
    // C1修正の回帰テスト: robots.txtが302を返すとき、reqwestの自動追従ではなく
    // webgrabが手動で1回だけ追従し、追従先のDisallowを適用する（exit 5）。
    let port_b = spawn_server(1, |_path| {
        http_response("User-agent: *\nDisallow: /", "text/plain")
    });
    let port = spawn_server(2, move |path| {
        if path == "/robots.txt" {
            return format!(
                "HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:{port_b}/robots.txt\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            );
        }
        http_response(ARTICLE, "text/html")
    });
    let url = format!("http://127.0.0.1:{port}/page");
    let (code, _stdout, stderr) = run_webgrab(&[&url, "--allow-private"]);
    assert_eq!(code, 5, "stderr={stderr}");
    assert!(stderr.contains("error=robots"));
}

#[test]
fn stdout_stderr_separation_on_success() {
    let port = spawn_server(3, |path| {
        if path == "/robots.txt" {
            return "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                .into();
        }
        http_response(ARTICLE, "text/html; charset=utf-8")
    });
    let url = format!("http://127.0.0.1:{port}/article");
    let (code, stdout, stderr) = run_webgrab(&[&url, "--allow-private"]);
    assert_eq!(code, 0);
    // 本文はstdout、stderrに本文が混じらない
    assert!(stdout.contains("統合テスト"));
    assert!(!stderr.contains("Markdown Content:"));
}

const EMPTY_SHELL: &str = "<html><head><title>CSR</title></head><body><div id=\"app\"></div><script>setTimeout(function(){document.getElementById('app').innerHTML='<p>late</p>'},500)</script></body></html>";

#[test]
fn empty_shell_exits_6_with_hint_token() {
    let port = spawn_server(3, |path| {
        if path == "/robots.txt" {
            return "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                .into();
        }
        http_response(EMPTY_SHELL, "text/html; charset=utf-8")
    });
    let url = format!("http://127.0.0.1:{port}/shell");
    let (code, _stdout, stderr) = run_webgrab(&[&url, "--allow-private"]);
    assert_eq!(code, 6, "stderr={stderr}");
    assert!(
        stderr
            .lines()
            .any(|l| l.starts_with("webgrab: error=empty hint=--render/--raw")),
        "{stderr}"
    );
    assert!(stderr.contains("warn=extract-grab-failed"));
}

#[test]
fn wait_ms_without_render_is_ignored_with_warning() {
    let port = spawn_server(3, |path| {
        if path == "/robots.txt" {
            return "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                .into();
        }
        http_response(ARTICLE, "text/html; charset=utf-8")
    });
    let url = format!("http://127.0.0.1:{port}/a");
    let (code, _stdout, stderr) = run_webgrab(&[&url, "--allow-private", "--wait-ms", "100"]);
    assert_eq!(code, 0);
    assert!(
        stderr.contains("warn=flag-ignored flag=--wait-ms"),
        "{stderr}"
    );
}

const SHORT_ARTICLE: &str = "<html><head><title>短い</title></head><body><article><p>これは百五十文字程度の短い本文です。抽出器が本文として認識できる長さはありますが、二百文字には届きません。エスカレーション判定の境界を確認するための固定文です。末尾。</p></article></body></html>";

#[test]
fn auto_render_skips_when_budget_is_short() {
    // --timeout 3 なら残余は常に5秒未満 → skip。Chrome不要。
    let port = spawn_server(3, |path| {
        if path == "/robots.txt" {
            return "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                .into();
        }
        http_response(SHORT_ARTICLE, "text/html; charset=utf-8")
    });
    let url = format!("http://127.0.0.1:{port}/short");
    let (code, stdout, stderr) =
        run_webgrab(&[&url, "--allow-private", "--auto-render", "--timeout", "3"]);
    assert_eq!(code, 0, "stderr={stderr}");
    assert!(
        stderr.contains("warn=auto-render-skipped reason=timeout"),
        "{stderr}"
    );
    let i_short = stdout
        .find("[webgrab:short-content")
        .expect("short-content");
    let i_rs = stdout
        .find("[webgrab:render-status skipped reason=timeout]")
        .expect("render-status");
    assert!(i_short < i_rs, "{stdout}");
    assert!(stdout.contains("retry with --render or --raw]"), "{stdout}");
}

#[test]
fn no_sandbox_warning_only_when_chrome_launches() {
    // Chromeを起動しない実行（エスカレーションしない静的ページ）では警告を出さない。
    let para = "これは十分に長い本文です。抽出アルゴリズムが本文と認識できるだけの日本語文章を用意し、可視テキストが二百文字を確実に超えるようにしています。";
    let body = format!(
        "<html><head><title>長い記事</title></head><body><article><h1>長い記事</h1><p>{para}</p><p>{para}</p><p>{para}</p></article></body></html>"
    );
    let page = body.clone();
    let port = spawn_server(4, move |path| {
        if path == "/robots.txt" {
            return http_response("User-agent: *\nAllow: /\n", "text/plain");
        }
        http_response(&page, "text/html; charset=utf-8")
    });
    let url = format!("http://127.0.0.1:{port}/long");
    let (code, _stdout, stderr) = run_webgrab(&[
        &url,
        "--allow-private",
        "--auto-render",
        "--no-sandbox",
        "--no-tokens",
    ]);
    assert_eq!(code, 0, "stderr={stderr}");
    assert!(!stderr.contains("warn=no-sandbox"), "{stderr}");
    assert!(!stderr.contains("info=auto-render"), "{stderr}");
}
