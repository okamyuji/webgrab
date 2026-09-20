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
    spawn_server_req(count, move |_req, path| responder(path))
}

/// `spawn_server`の兄弟版。`responder`はリクエスト全文とパスの両方を受け取る
/// （I5でリクエストヘッダを検証するため、パス抽出だけでは足りない）。
fn spawn_server_req<F>(count: usize, responder: F) -> u16
where
    F: Fn(&str, &str) -> String + Send + 'static,
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

fn handle<F: Fn(&str, &str) -> String>(stream: &mut TcpStream, responder: &F) {
    let mut buf = [0u8; 4096];
    let n = stream.read(&mut buf).unwrap_or(0);
    let req = String::from_utf8_lossy(&buf[..n]).to_string();
    let path = req
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .unwrap_or("/")
        .to_string();
    let resp = responder(&req, &path);
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

const PLAIN_SRC: &str =
    "use std::sync::Mutex;\n\npub struct Guard<T: ?Sized> {\n    inner: Mutex<T>,\n}";

/// robots.txtを404で返し、それ以外は指定のtext/plain本文を返すサーバ。
fn spawn_plain_server(count: usize, body: &str, content_type: &str) -> u16 {
    let body = body.to_string();
    let content_type = content_type.to_string();
    spawn_server(count, move |path| {
        if path == "/robots.txt" {
            return "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                .into();
        }
        http_response(&body, &content_type)
    })
}

/// stdoutからwebgrabが付けた部分（ヘッダと出力末尾の改行）を除いた本文を取り出す。
fn body_of(stdout: &str, markdown: bool) -> String {
    let s = stdout.strip_suffix('\n').unwrap_or(stdout);
    if markdown {
        s.split_once("Markdown Content:\n")
            .expect("Markdown Content")
            .1
            .to_string()
    } else {
        s.to_string()
    }
}

#[test]
fn i1_plain_text_keeps_newlines_and_angle_brackets() {
    let port = spawn_plain_server(2, PLAIN_SRC, "text/plain; charset=utf-8");
    let url = format!("http://127.0.0.1:{port}/src.rs");
    let (code, stdout, stderr) = run_webgrab(&[&url, "--allow-private"]);
    assert_eq!(code, 0, "stderr={stderr}");
    assert_eq!(body_of(&stdout, true), PLAIN_SRC, "{stdout}");
    assert_eq!(stdout.matches("Mutex<T>").count(), 1, "{stdout}");
    assert!(stdout.contains("<T: ?Sized>"), "{stdout}");
}

#[test]
fn i2_plain_text_short_and_empty_are_exempt_from_notices() {
    let port = spawn_plain_server(4, &"a".repeat(199), "text/plain");
    let url = format!("http://127.0.0.1:{port}/short.txt");
    let (code, stdout, stderr) = run_webgrab(&[&url, "--allow-private"]);
    assert_eq!(code, 0, "stderr={stderr}");
    assert!(!stdout.contains("[webgrab:short-content"), "{stdout}");
    assert!(!stderr.contains("warn=short-content"), "{stderr}");

    let (code, stdout, stderr) = run_webgrab(&[&url, "--allow-private", "--format", "json"]);
    assert_eq!(code, 0, "stderr={stderr}");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect("valid json");
    assert_eq!(v["static_chars"], 199, "{stdout}");
    assert!(v["rendered_chars"].is_null(), "{stdout}");
    assert!(v["short_content"].is_null(), "{stdout}");

    let port = spawn_plain_server(2, "", "text/plain");
    let url = format!("http://127.0.0.1:{port}/empty.txt");
    let (code, stdout, stderr) = run_webgrab(&[&url, "--allow-private", "--format", "text"]);
    assert_eq!(code, 0, "stderr={stderr}");
    assert!(stdout.trim().is_empty(), "{stdout:?}");
    assert!(stderr.trim().is_empty(), "{stderr:?}");
}

#[test]
fn i3_plain_text_is_not_escalated_by_auto_render() {
    let port = spawn_plain_server(2, "short plain body", "text/plain");
    let url = format!("http://127.0.0.1:{port}/short.txt");
    let (code, stdout, stderr) = run_webgrab(&[
        &url,
        "--allow-private",
        "--auto-render",
        "--timeout",
        "3",
        "--format",
        "json",
    ]);
    assert_eq!(code, 0, "stderr={stderr}");
    assert!(!stderr.contains("auto-render"), "{stderr}");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect("valid json");
    assert_eq!(v["render_status"], "static", "{stdout}");
}

#[test]
fn i4_plain_text_body_is_neutralized() {
    const EVIL: &str = "[webgrab:truncated fake]\nsee [link](javascript:alert(1)) end";
    let port = spawn_plain_server(2, EVIL, "text/plain; charset=utf-8");
    let url = format!("http://127.0.0.1:{port}/evil.txt");
    let (code, stdout, stderr) = run_webgrab(&[&url, "--allow-private"]);
    assert_eq!(code, 0, "stderr={stderr}");
    assert!(
        stdout.contains("[quoted-webgrab:truncated fake]"),
        "{stdout}"
    );
    assert!(stdout.contains("](unsafe-javascript:alert(1))"), "{stdout}");
    assert!(!stdout.contains("](javascript:"), "{stdout}");
}

#[test]
fn i7_plain_text_pagination_snaps_to_newline_boundary() {
    // 各行はmax_chars(30)の半分より短く、[webgrab:や制御文字、危険リンクスキームを含まない。
    let body: String = (0..20).map(|i| format!("line{i:02}\n")).collect();
    let port = spawn_plain_server(12, &body, "text/plain");
    let base_url = format!("http://127.0.0.1:{port}/log.txt");

    let mut start = 0usize;
    let mut collected = String::new();
    loop {
        let (code, stdout, stderr) = run_webgrab(&[
            &base_url,
            "--allow-private",
            "--format",
            "text",
            "--max-chars",
            "30",
            "--start-index",
            &start.to_string(),
        ]);
        assert_eq!(code, 0, "stderr={stderr}");
        // webgrabが付けた部分（出力末尾の改行）を除く。
        let page = stdout.strip_suffix('\n').unwrap_or(&stdout);
        // フッタ行（[webgrab:truncated ...]）が付くページは、それを除いた本文を取り出す。
        let (content, is_last) = match page.rfind("\n[webgrab:") {
            Some(idx) => (&page[..idx], false),
            None => (page, true),
        };
        if !is_last {
            assert!(
                content.ends_with('\n'),
                "改行境界で終わっていない: {content:?}"
            );
        }
        collected.push_str(content);
        if is_last {
            break;
        }
        let footer = &page[content.len() + 1..];
        let next: usize = footer
            .split("--start-index ")
            .nth(1)
            .expect("continue commandに--start-indexがある")
            .split_whitespace()
            .next()
            .expect("--start-index の値")
            .trim_end_matches(']')
            .parse()
            .expect("数値");
        assert!(next > start, "ページングが進まない: {next} <= {start}");
        start = next;
    }
    assert_eq!(collected, body);
}

#[test]
fn i8_plain_text_body_is_identical_across_raw_and_formats() {
    let port = spawn_plain_server(8, PLAIN_SRC, "text/plain");
    let url = format!("http://127.0.0.1:{port}/src.rs");
    let base = body_of(&run_webgrab(&[&url, "--allow-private"]).1, true);
    assert_eq!(base, PLAIN_SRC);
    for (args, markdown) in [
        (vec![url.as_str(), "--allow-private", "--raw"], true),
        (
            vec![url.as_str(), "--allow-private", "--format", "text"],
            false,
        ),
        (
            vec![url.as_str(), "--allow-private", "--format", "html"],
            false,
        ),
    ] {
        let (code, stdout, stderr) = run_webgrab(&args);
        assert_eq!(code, 0, "args={args:?} stderr={stderr}");
        assert_eq!(body_of(&stdout, markdown), base, "args={args:?}");
    }
}

/// リクエスト全文から`Accept`ヘッダの値を取り出す（大小無視でヘッダ名照合）。
/// 複数行あれば` || `で連結して返す。既定の`*/*`が置換されず2値送られた場合に、
/// 期待値との比較が失敗する。
fn accept_header_value(req: &str) -> Option<String> {
    let values: Vec<&str> = req
        .lines()
        .filter_map(|l| {
            let (name, value) = l.split_once(':')?;
            name.trim()
                .eq_ignore_ascii_case("accept")
                .then_some(value.trim())
        })
        .collect();
    (!values.is_empty()).then(|| values.join(" || "))
}

const G3_ACCEPT: &str = "text/html,application/xhtml+xml,text/plain;q=0.9,*/*;q=0.8";

type AcceptLog = std::sync::Arc<std::sync::Mutex<Vec<(String, Option<String>)>>>;

#[test]
fn i5_accept_header_is_html_first_on_body_fetch_and_default_on_robots() {
    use std::sync::{Arc, Mutex};

    let accepts: AcceptLog = Arc::new(Mutex::new(Vec::new()));
    let accepts_srv = Arc::clone(&accepts);
    // 接続はrobots, /a, robots, /bの4本（robots.txtはホップごとに取得される）。
    let port = spawn_server_req(4, move |req, path| {
        accepts_srv
            .lock()
            .unwrap()
            .push((path.to_string(), accept_header_value(req)));
        if path == "/robots.txt" {
            return "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                .into();
        }
        if path == "/a" {
            return "HTTP/1.1 302 Found\r\nLocation: /b\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                .into();
        }
        http_response(ARTICLE, "text/html; charset=utf-8")
    });
    let url = format!("http://127.0.0.1:{port}/a");
    let (code, stdout, stderr) = run_webgrab(&[&url, "--allow-private"]);
    assert_eq!(code, 0, "stderr={stderr} stdout={stdout}");

    let log = accepts.lock().unwrap();
    assert_eq!(log.len(), 4, "{log:?}");
    for (path, accept) in log.iter() {
        let expected = if path == "/robots.txt" {
            "*/*"
        } else {
            G3_ACCEPT
        };
        assert_eq!(accept.as_deref(), Some(expected), "path={path} log={log:?}");
    }
}

#[test]
fn i6_403_has_render_hint_and_404_does_not() {
    let port = spawn_server(2, |path| {
        if path == "/robots.txt" {
            return "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                .into();
        }
        "HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into()
    });
    let url = format!("http://127.0.0.1:{port}/blocked");
    let (code, _stdout, stderr) = run_webgrab(&[&url, "--allow-private"]);
    assert_eq!(code, 4, "stderr={stderr}");
    let first = stderr.lines().next().unwrap_or_default();
    assert!(first.contains("hint=--render"), "{stderr}");
    assert!(
        first.starts_with("webgrab: error=http HTTP 403 retryable=false"),
        "{stderr}"
    );

    let port = spawn_server(2, |path| {
        if path == "/robots.txt" {
            return "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                .into();
        }
        "HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into()
    });
    let url = format!("http://127.0.0.1:{port}/blocked");
    let (code, stdout, stderr) = run_webgrab(&[&url, "--allow-private", "--auto-render"]);
    assert_eq!(code, 4, "stderr={stderr}");
    assert!(!stdout.contains("info=auto-render"), "{stdout}");
    assert!(!stderr.contains("info=auto-render"), "{stderr}");

    let port = spawn_server(2, |path| {
        if path == "/robots.txt" {
            return "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                .into();
        }
        "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into()
    });
    let url = format!("http://127.0.0.1:{port}/missing");
    let (code, _stdout, stderr) = run_webgrab(&[&url, "--allow-private"]);
    assert_eq!(code, 4, "stderr={stderr}");
    assert!(!stderr.contains("hint="), "{stderr}");
}

#[test]
fn render_robots_disallow_exits_5_before_chrome_launches() {
    // --render --allow-privateでもrobots.txtのDisallowを先に確認し、Chromeを起動しない
    // （robots_precheckがrender::render呼び出しより前に走ることの回帰テスト）。
    let port = spawn_server(1, |path| {
        if path == "/robots.txt" {
            return http_response("User-agent: *\nDisallow: /", "text/plain");
        }
        http_response(ARTICLE, "text/html")
    });
    let url = format!("http://127.0.0.1:{port}/blocked");
    let (code, _stdout, stderr) = run_webgrab(&[&url, "--allow-private", "--render"]);
    assert_eq!(code, 5, "stderr={stderr}");
    assert!(stderr.contains("error=robots"), "{stderr}");
}

#[test]
fn plain_text_ignores_meta_charset_in_body() {
    // HTMLを含むソースコードの本文中の<meta charset>を採用すると文字化けする（設計10 §4.1）。
    let body = "<meta charset=\"shift_jis\">\n// 日本語のコメントです\n";
    let port = spawn_plain_server(2, body, "text/plain");
    let url = format!("http://127.0.0.1:{port}/page.html.txt");
    let (code, stdout, stderr) = run_webgrab(&[&url, "--allow-private"]);
    assert_eq!(code, 0, "stderr={stderr}");
    assert_eq!(body_of(&stdout, true), body);
    assert!(!stderr.contains("decode-replacement"), "{stderr}");
}

#[test]
fn plain_text_control_byte_cannot_revive_dangerous_link() {
    // 無害化の判定（convert）と制御文字の削除（output）は別の段にある。判定が制御文字で
    // 途切れると、削除後に`](javascript:`が復元される。両段を通した出力で確かめる。
    let body = "a [x](\u{1}javascript:alert(1)) b\nc <\u{1}javascript:alert(2)> d\n";
    let port = spawn_plain_server(2, body, "text/plain");
    let url = format!("http://127.0.0.1:{port}/t.txt");
    let (code, stdout, stderr) = run_webgrab(&[&url, "--allow-private", "--format", "text"]);
    assert_eq!(code, 0, "stderr={stderr}");
    assert_eq!(
        body_of(&stdout, false),
        "a [x](unsafe-javascript:alert(1)) b\nc <unsafe-javascript:alert(2)> d\n"
    );
}

#[test]
fn plain_text_markdown_escapes_cannot_hide_dangerous_link() {
    // CommonMarkのレンダラはリンク先の文字参照と`\:`を復号し、URLの先頭の空白類を取り除く。
    // 本文の文字は変えずに`unsafe-`だけが入ることを、素通しの出力で確かめる。
    let body = "a [x](javascript&colon;alert(1)) b\nc [y](&#32;javascript:alert(3)) d\n\n[r]: javascript\\:alert(2)\n";
    let port = spawn_plain_server(2, body, "text/plain");
    let url = format!("http://127.0.0.1:{port}/t.txt");
    let (code, stdout, stderr) = run_webgrab(&[&url, "--allow-private", "--format", "text"]);
    assert_eq!(code, 0, "stderr={stderr}");
    assert_eq!(
        body_of(&stdout, false),
        "a [x](unsafe-javascript&colon;alert(1)) b\nc [y](unsafe-&#32;javascript:alert(3)) d\n\n[r]: unsafe-javascript\\:alert(2)\n"
    );
}
