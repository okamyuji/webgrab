//! HTTP取得（設計§3.1, §4 fetch）。
//! IPピン留め・手動リダイレクト追従・netguard/robotsの結線を担う。

use crate::content_encoding::{self, Coding, DecodeError, Decoder};
use crate::error::{ExitCode, Result, WebgrabError};
use crate::{netguard, robots::Robots};
use std::net::{SocketAddr, ToSocketAddrs};
use std::time::Duration;
use url::Url;

const MAX_HOPS: usize = 10;
const ROBOTS_MAX_BYTES: usize = 512 * 1024;

/// 本文取得のAcceptヘッダ（G3）。HTMLを優先し、reqwest既定の`*/*`を置き換える。
/// robots.txtの取得には適用しない（既定の`*/*`のまま）。
const BODY_ACCEPT: &str = "text/html,application/xhtml+xml,text/plain;q=0.9,*/*;q=0.8";

/// 対応メディアタイプか（大小無視）。空は許可（Content-Type欠落サーバ向け）。
fn is_supported_media_type(main: &str) -> bool {
    let m = main.to_ascii_lowercase();
    m.is_empty() || m == "text/html" || m == "application/xhtml+xml" || m == "text/plain"
}

/// 静的経路のHTTPエラーメッセージ（設計§4.5）。5xxと429は再試行可能。
/// 403だけ`hint=--render`を末尾に付ける（stderr先頭行の`error=<token> <message>`書式は変わらない）。
pub fn http_error_message(status: u16) -> String {
    let retryable = (500..600).contains(&status) || status == 429;
    let mut msg = format!("HTTP {status} retryable={retryable}");
    if status == 403 {
        msg.push_str(" hint=--render");
    }
    msg
}

/// Content-Typeの主タイプが`text/plain`か（大小無視、`; charset=`等のパラメータは無視）。
pub fn is_plain_text(content_type: Option<&str>) -> bool {
    content_type.is_some_and(|ct| {
        ct.split(';')
            .next()
            .unwrap_or("")
            .trim()
            .eq_ignore_ascii_case("text/plain")
    })
}

/// 取得結果。
pub struct Fetched {
    pub final_url: String,
    pub content_type: Option<String>,
    pub body: Vec<u8>,
    /// 最終応答の展開後バイト数（残余--max-bytesの計算に使う）。
    pub consumed_bytes: u64,
}

/// オプション（fetch層が必要とする分だけ）。
pub struct FetchOptions {
    pub user_agent: String,
    pub timeout: Duration,
    pub max_bytes: u64,
    pub allow_private: bool,
    pub check_robots: bool,
    /// `application/xml`と`text/xml`も受ける（`--map`のsitemap取得だけで使う）。
    pub allow_xml: bool,
    /// `Some(home)`なら、転送は今のホストか`home`へだけたどる（`--map`のsitemap取得で
    /// 起点のホストを渡す）。悪意あるsitemapが第三者へ転送すると、sitemapの件数×転送の段数×
    /// robots確認の分だけ第三者へ要求が届く。起点のホストは利用者が選んだ相手なので許す
    /// （`example.com`のsitemapを`www.example.com`へ転送するサイトがある）。
    pub redirect_home: Option<String>,
}

/// ホスト名を解決し、netguardを通した検証済みSocketAddrを返す。
/// 名前解決(getaddrinfo)は全体予算`timeout`で囲む。悪意あるDNSの無応答で
/// プロセスが無期限ハングするのを防ぐ。
async fn resolve_checked(
    host: &str,
    port: u16,
    allow_private: bool,
    timeout: Duration,
) -> Result<SocketAddr> {
    // `Url::host_str`はIPv6を`[::1]`と角括弧付きで返すが、`to_socket_addrs`は角括弧を解さない。
    let host_owned = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host)
        .to_string();
    let resolve = tokio::task::spawn_blocking(move || {
        (host_owned.as_str(), port)
            .to_socket_addrs()
            .map(|it| it.collect::<Vec<_>>())
    });
    let addrs: Vec<SocketAddr> = tokio::time::timeout(timeout, resolve)
        .await
        .map_err(|_| {
            WebgrabError::new(
                ExitCode::Network,
                format!("DNS resolution timed out for {host}"),
            )
        })?
        .map_err(|e| {
            WebgrabError::new(ExitCode::Internal, "resolver task failed").with_detail(e.to_string())
        })?
        .map_err(|e| {
            WebgrabError::new(
                ExitCode::Network,
                format!("DNS resolution failed for {host}"),
            )
            .with_detail(e.to_string())
        })?;
    if addrs.is_empty() {
        return Err(WebgrabError::new(
            ExitCode::Network,
            format!("no address for {host}"),
        ));
    }
    if !allow_private {
        for a in &addrs {
            if let Some(range) = netguard::deny_range(a.ip()) {
                return Err(
                    WebgrabError::new(ExitCode::Netguard, "refused internal address").with_detail(
                        format!(
                            "host={host} resolved={} range={range} (use --allow-private to override)",
                            a.ip()
                        ),
                    ),
                );
            }
        }
    }
    Ok(addrs[0])
}

/// robots.txtを取得して判定する。取得失敗・サイズ超過は「許可」とみなす。
/// リダイレクトは自動追従せず、最大1回だけ手動追従し追従先もnetguardで再検証する（C1）。
/// `addr`は呼び出し側で検証済みの初回ホストのピン留めIP（M4、二重解決を避ける）。
async fn robots_allowed(url: &Url, addr: SocketAddr, opts: &FetchOptions) -> Result<bool> {
    let authority = match url.port() {
        Some(p) => format!("{}:{}", url.host_str().unwrap_or_default(), p),
        None => url.host_str().unwrap_or_default().to_string(),
    };
    let robots_url = format!("{}://{}/robots.txt", url.scheme(), authority);
    let path = url.path().to_string();

    let mut target = robots_url;
    let mut pinned_host = url.host_str().unwrap_or_default().to_string();
    let mut pinned_addr = addr;

    for attempt in 0..2 {
        let client = reqwest::Client::builder()
            .user_agent(&opts.user_agent)
            .default_headers(accept_encoding_header())
            .timeout(Duration::from_secs(5))
            .redirect(reqwest::redirect::Policy::none())
            .resolve(&pinned_host, pinned_addr)
            .build()
            .map_err(|e| {
                WebgrabError::new(ExitCode::Internal, "client build failed")
                    .with_detail(e.to_string())
            })?;

        let resp = match client.get(&target).send().await {
            Ok(r) => r,
            Err(_) => return Ok(true), // 取得失敗 = 許可
        };
        let status = resp.status();
        if status.is_redirection() && attempt == 0 {
            let Some(next) = robots_redirect_target(&resp, &target) else {
                return Ok(true);
            };
            // 本文の転送と同じ制限を掛ける。たどれない転送は取得失敗と同じく許可扱い。
            let from = Url::parse(&target).ok();
            if from.is_none_or(|f| !redirect_allowed(opts.redirect_home.as_deref(), &f, &next)) {
                return Ok(true);
            }
            let host = next.host_str().unwrap_or_default().to_string();
            let port = next.port_or_known_default().unwrap_or(80);
            // 追従先を再解決・再検証（内部アドレスなら拒否）
            pinned_addr = resolve_checked(&host, port, opts.allow_private, opts.timeout).await?;
            pinned_host = host;
            target = next.to_string();
            continue;
        }
        if !status.is_success() {
            return Ok(true);
        }
        // 本文取得と同様にストリーミングで上限を適用する。全体を先読みしないことで
        // 巨大/無限ストリームな robots.txt によるメモリ枯渇(DoS)を防ぐ。
        let bytes = match read_capped_robots(resp, ROBOTS_MAX_BYTES).await {
            Some(b) => b,
            None => return Ok(true), // 取得失敗 or 上限超過 = 許可扱い
        };
        let text = String::from_utf8_lossy(&bytes);
        let rules = Robots::parse(&text);
        return Ok(rules.allowed(&path));
    }
    Ok(true)
}

/// robots.txtの3xx応答からリダイレクト先を求める。追従できない応答は`None`（許可扱い）。
fn robots_redirect_target(resp: &reqwest::Response, target: &str) -> Option<Url> {
    let loc = resp
        .headers()
        .get(reqwest::header::LOCATION)?
        .to_str()
        .ok()?;
    let next = Url::parse(target).and_then(|b| b.join(loc)).ok()?;
    netguard::is_allowed_scheme(next.scheme()).then_some(next)
}

/// 対象URL単体のrobots.txt許可判定（render経路用）。静的経路と同じ範囲＝トップURLのみを
/// 確認する。ホスト解決とnetguard検証も行う。`--no-robots`相当のスキップは呼び出し側の責務。
pub async fn robots_precheck(url_str: &str, opts: &FetchOptions) -> Result<bool> {
    let url = Url::parse(url_str).map_err(|e| {
        WebgrabError::new(ExitCode::Usage, "invalid URL").with_detail(e.to_string())
    })?;
    let (_, addr) = resolve_url(&url, opts).await?;
    robots_allowed(&url, addr, opts).await
}

/// URLのスキームを検査し、ホストを解決してnetguardを通したSocketAddrとホスト名を返す。
async fn resolve_url(url: &Url, opts: &FetchOptions) -> Result<(String, SocketAddr)> {
    if !netguard::is_allowed_scheme(url.scheme()) {
        return Err(WebgrabError::new(
            ExitCode::Usage,
            format!("unsupported scheme: {}", url.scheme()),
        ));
    }
    let host = url
        .host_str()
        .ok_or_else(|| WebgrabError::new(ExitCode::Usage, "URL has no host"))?
        .to_string();
    let port = url.port_or_known_default().unwrap_or(80);
    let addr = resolve_checked(&host, port, opts.allow_private, opts.timeout).await?;
    Ok((host, addr))
}

/// 本文取得用クライアントを構築する。`Accept`を§4.3の値に固定する（G3）。
/// `default_headers`は`insert`で既定の`Accept: */*`を置き換えるため、送るAcceptは1値だけになる。
fn build_body_client(opts: &FetchOptions, host: &str, addr: SocketAddr) -> Result<reqwest::Client> {
    let mut headers = accept_encoding_header();
    headers.insert(
        reqwest::header::ACCEPT,
        reqwest::header::HeaderValue::from_static(BODY_ACCEPT),
    );
    reqwest::Client::builder()
        .user_agent(&opts.user_agent)
        .timeout(opts.timeout)
        .redirect(reqwest::redirect::Policy::none())
        .resolve(host, addr)
        .default_headers(headers)
        .build()
        .map_err(|e| {
            WebgrabError::new(ExitCode::Internal, "client build failed").with_detail(e.to_string())
        })
}

/// 自前で展開する方式だけを提示する（reqwestの自動展開は使わない）。
fn accept_encoding_header() -> reqwest::header::HeaderMap {
    let mut headers = reqwest::header::HeaderMap::with_capacity(2);
    headers.insert(
        reqwest::header::ACCEPT_ENCODING,
        reqwest::header::HeaderValue::from_static(content_encoding::ACCEPT_ENCODING),
    );
    headers
}

/// Content-Encodingの全行・全値から展開方式を決める。展開できなければ`ExitCode::Http`で拒否する。
fn coding_of_response(resp: &reqwest::Response) -> Result<Coding> {
    let values = resp.headers().get_all(reqwest::header::CONTENT_ENCODING);
    content_encoding::coding_of(values.iter().map(|v| v.as_bytes())).map_err(|enc| {
        WebgrabError::new(
            ExitCode::Http,
            format!("unsupported content-encoding: {enc}"),
        )
    })
}

/// メディアタイプが対応外なら`ExitCode::Http`で拒否する（メディアタイプは大小無視: RFC 9110 §8.3.1）。
fn check_media_type(content_type: Option<&str>, allow_xml: bool) -> Result<()> {
    let Some(ct) = content_type else {
        return Ok(());
    };
    let main = ct.split(';').next().unwrap_or("").trim();
    let xml = allow_xml
        && (main.eq_ignore_ascii_case("application/xml") || main.eq_ignore_ascii_case("text/xml"));
    if !xml && !is_supported_media_type(main) {
        return Err(WebgrabError::new(
            ExitCode::Http,
            format!("unsupported content-type: {main}"),
        ));
    }
    Ok(())
}

/// 3xx応答のLocationを現在URL基準で解決する。
fn redirect_target(resp: &reqwest::Response, current: &Url) -> Result<Url> {
    let location = resp
        .headers()
        .get(reqwest::header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| WebgrabError::new(ExitCode::Network, "redirect without location"))?;
    current.join(location).map_err(|e| {
        WebgrabError::new(ExitCode::Network, "invalid redirect location").with_detail(e.to_string())
    })
}

/// 非3xx応答を本文読み込みの前に検査し、Content-Typeと展開方式を返す。
fn accept_response(resp: &reqwest::Response, allow_xml: bool) -> Result<(Option<String>, Coding)> {
    let status = resp.status();
    if status.is_client_error() || status.is_server_error() {
        return Err(WebgrabError::new(
            ExitCode::Http,
            http_error_message(status.as_u16()),
        ));
    }
    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    check_media_type(content_type.as_deref(), allow_xml)?;
    Ok((content_type, coding_of_response(resp)?))
}

fn redirect_allowed(home: Option<&str>, current: &Url, next: &Url) -> bool {
    home.is_none_or(|h| next.host_str() == current.host_str() || next.host_str() == Some(h))
}

/// 静的取得を手動リダイレクト追従で行う（各ホップでnetguard再適用）。`opts.timeout`は
/// 名前解決・robots確認・転送・本文読み込みの全体に掛ける。各段に渡すだけでは段ごとに
/// 同じ時間を使えてしまい、転送が続くと合計が`--timeout`を超える。
pub async fn fetch(url_str: &str, opts: &FetchOptions) -> Result<Fetched> {
    tokio::time::timeout(opts.timeout, fetch_hops(url_str, opts))
        .await
        .unwrap_or_else(|_| {
            Err(WebgrabError::new(ExitCode::Network, "request timed out")
                .with_detail(format!("url={url_str} timeout={:?}", opts.timeout)))
        })
}

async fn fetch_hops(url_str: &str, opts: &FetchOptions) -> Result<Fetched> {
    let mut current = Url::parse(url_str).map_err(|e| {
        WebgrabError::new(ExitCode::Usage, "invalid URL").with_detail(e.to_string())
    })?;

    for _ in 0..MAX_HOPS {
        let (host, addr) = resolve_url(&current, opts).await?;

        // robots確認（各ホップの着地ホストに対して、解決済みaddrを再利用）
        if opts.check_robots && !robots_allowed(&current, addr, opts).await? {
            return Err(WebgrabError::new(ExitCode::Robots, "blocked by robots.txt")
                .with_detail(format!("url={current}")));
        }

        let client = build_body_client(opts, &host, addr)?;

        let resp = client.get(current.as_str()).send().await.map_err(|e| {
            WebgrabError::new(ExitCode::Network, "request failed").with_detail(e.to_string())
        })?;

        if resp.status().is_redirection() {
            let next = redirect_target(&resp, &current)?;
            if !redirect_allowed(opts.redirect_home.as_deref(), &current, &next) {
                return Err(WebgrabError::new(
                    ExitCode::Network,
                    "redirect to another host refused",
                )
                .with_detail(format!("from={current} to={next}")));
            }
            current = next;
            continue;
        }
        let (content_type, coding) = accept_response(&resp, opts.allow_xml)?;

        // ストリーミング読み込みで max_bytes を展開後サイズに適用
        let body = read_capped(resp, coding, opts.max_bytes).await?;
        return Ok(Fetched {
            final_url: current.to_string(),
            content_type,
            consumed_bytes: body.len() as u64,
            body,
        });
    }
    Err(WebgrabError::new(ExitCode::Network, "too many redirects"))
}

/// robots.txt をチャンク単位で読み、上限超過・読み取り失敗時は None を返す。
/// 上限超過は「大きすぎる robots は許可扱い」というポリシーに合わせて呼び出し側で処理する。
/// 展開できない符号化も、読み取り失敗・展開失敗と同じく`None`（許可扱い）にする。
async fn read_capped_robots(mut resp: reqwest::Response, max_bytes: usize) -> Option<Vec<u8>> {
    let coding = coding_of_response(&resp).ok()?;
    let mut decoder = Decoder::new(coding, max_bytes as u64);
    while let Some(chunk) = resp.chunk().await.ok()? {
        decoder.push(&chunk).ok()?;
    }
    decoder.finish().ok()
}

async fn read_capped(
    mut resp: reqwest::Response,
    coding: Coding,
    max_bytes: u64,
) -> Result<Vec<u8>> {
    // 失敗したときも、それまでに展開した量を呼び出し側の残りから引けるよう返す。
    // --max-bytes は展開後の量なので、回線上の量ではなく展開後の量で数える。
    // 上限を超えた取得は上限まで読み終えている。
    let decode_err = |e: DecodeError, decoded: u64| match e {
        DecodeError::OverCap => WebgrabError::new(
            ExitCode::Http,
            format!("response exceeds --max-bytes ({max_bytes})"),
        )
        .with_consumed(max_bytes),
        DecodeError::Corrupt(d) => WebgrabError::new(ExitCode::Network, "stream read failed")
            .with_detail(d)
            .with_consumed(decoded),
    };
    let mut decoder = Decoder::new(coding, max_bytes);
    loop {
        let chunk = match resp.chunk().await {
            Ok(Some(chunk)) => chunk,
            Ok(None) => break,
            Err(e) => {
                return Err(WebgrabError::new(ExitCode::Network, "stream read failed")
                    .with_detail(e.to_string())
                    .with_consumed(decoder.decoded_len()));
            }
        };
        if let Err(e) = decoder.push(&chunk) {
            return Err(decode_err(e, decoder.decoded_len()));
        }
    }
    let decoded = decoder.decoded_len();
    decoder.finish().map_err(|e| decode_err(e, decoded))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn resolve_rejects_localhost() {
        let e = resolve_checked("localhost", 80, false, Duration::from_secs(5))
            .await
            .unwrap_err();
        assert_eq!(e.code, ExitCode::Netguard);
        let d = e.detail.unwrap_or_default();
        assert!(d.contains("host=localhost"), "{d}");
        assert!(d.contains("resolved="), "{d}");
        assert!(d.contains("range=loopback"), "{d}");
    }

    #[test]
    fn media_type_check_is_case_insensitive() {
        assert!(is_supported_media_type("TEXT/HTML"));
        assert!(is_supported_media_type("Text/Html"));
        assert!(is_supported_media_type("application/XHTML+XML"));
        assert!(is_supported_media_type("")); // Content-Type欠落は許可
        assert!(!is_supported_media_type("application/pdf"));
        assert!(!is_supported_media_type("image/png"));
    }

    #[test]
    fn check_media_type_rejects_unsupported_with_exit_4() {
        assert!(check_media_type(None, false).is_ok());
        assert!(check_media_type(Some("text/html; charset=utf-8"), false).is_ok());
        assert!(check_media_type(Some("text/plain"), false).is_ok());
        let e = check_media_type(Some("application/pdf; q=1"), false).unwrap_err();
        assert_eq!(e.code, ExitCode::Http);
        assert_eq!(e.message, "unsupported content-type: application/pdf");
    }

    #[tokio::test]
    async fn resolve_checked_accepts_a_bracketed_ipv6_literal() {
        let addr = resolve_checked("[::1]", 8080, true, Duration::from_secs(2))
            .await
            .unwrap();
        assert_eq!(addr, "[::1]:8080".parse::<SocketAddr>().unwrap());
        let e = resolve_checked("[::1]", 8080, false, Duration::from_secs(2))
            .await
            .unwrap_err();
        assert_eq!(e.code, ExitCode::Netguard);
    }

    #[test]
    fn redirect_allowed_keeps_sitemaps_on_the_current_or_home_host() {
        let u = |s: &str| Url::parse(s).unwrap();
        let cur = u("https://example.com/sitemap.xml");
        assert!(redirect_allowed(None, &cur, &u("https://third.test/x")));
        let home = Some("www.example.com");
        assert!(redirect_allowed(
            home,
            &cur,
            &u("http://example.com:8080/s.xml")
        ));
        assert!(redirect_allowed(
            home,
            &cur,
            &u("https://www.example.com/sitemap.xml")
        ));
        assert!(!redirect_allowed(home, &cur, &u("https://third.test/x")));
        assert!(!redirect_allowed(
            home,
            &cur,
            &u("https://sub.www.example.com/x")
        ));
    }

    #[test]
    fn check_media_type_accepts_xml_only_when_allowed() {
        for ct in ["application/xml", "Text/XML; charset=utf-8"] {
            assert!(check_media_type(Some(ct), true).is_ok(), "{ct}");
            assert!(check_media_type(Some(ct), false).is_err(), "{ct}");
        }
        assert!(check_media_type(Some("text/html"), true).is_ok());
        assert!(check_media_type(Some("application/pdf"), true).is_err());
        assert!(check_media_type(Some("application/rss+xml"), true).is_err());
    }

    #[test]
    fn plain_text_detection_ignores_case_and_parameters() {
        assert!(is_plain_text(Some("text/plain")));
        assert!(is_plain_text(Some("Text/PLAIN")));
        assert!(is_plain_text(Some("text/plain; charset=utf-8")));
        assert!(is_plain_text(Some("  text/plain  ; charset=shift_jis")));
        assert!(!is_plain_text(Some("text/html")));
        assert!(!is_plain_text(Some("text/plain-ish")));
        assert!(!is_plain_text(Some("application/xhtml+xml")));
        assert!(!is_plain_text(None));
    }

    #[test]
    fn http_error_message_adds_render_hint_only_for_403() {
        assert_eq!(
            http_error_message(403),
            "HTTP 403 retryable=false hint=--render"
        );
        assert_eq!(http_error_message(404), "HTTP 404 retryable=false");
        assert_eq!(http_error_message(429), "HTTP 429 retryable=true");
        assert_eq!(http_error_message(500), "HTTP 500 retryable=true");
    }

    fn opts() -> FetchOptions {
        FetchOptions {
            user_agent: "webgrab-test".into(),
            timeout: Duration::from_secs(5),
            max_bytes: 1024,
            allow_private: false,
            check_robots: true,
            allow_xml: false,
            redirect_home: None,
        }
    }

    #[tokio::test]
    async fn resolve_url_rejects_unsupported_scheme_with_usage() {
        let url = Url::parse("ftp://example.com/x").unwrap();
        let e = resolve_url(&url, &opts()).await.unwrap_err();
        assert_eq!(e.code, ExitCode::Usage);
        assert_eq!(e.message, "unsupported scheme: ftp");
    }

    // fetchは各ホップのaddrをresolve_urlからしか得ないため、これがリダイレクト先の内部アドレス拒否を担う。
    #[tokio::test]
    async fn resolve_url_rejects_internal_address_without_allow_private() {
        let url = Url::parse("http://169.254.169.254/latest/meta-data/").unwrap();
        let e = resolve_url(&url, &opts()).await.unwrap_err();
        assert_eq!(e.code, ExitCode::Netguard);
        assert!(
            e.detail
                .unwrap_or_default()
                .contains("resolved=169.254.169.254"),
        );
    }

    #[tokio::test]
    async fn robots_precheck_rejects_invalid_url_with_usage() {
        let e = robots_precheck("not a url", &opts()).await.unwrap_err();
        assert_eq!(e.code, ExitCode::Usage);
        assert_eq!(e.message, "invalid URL");
    }

    #[tokio::test]
    async fn resolve_allows_localhost_with_flag() {
        // allow_private=true なら通る（解決自体は成功する前提のためエラーでもコード違い）
        let r = resolve_checked("localhost", 80, true, Duration::from_secs(5)).await;
        assert!(r.is_ok() || r.unwrap_err().code == ExitCode::Network);
    }
}
