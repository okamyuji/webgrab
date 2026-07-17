//! HTTP取得（設計§3.1, §4 fetch）。
//! IPピン留め・手動リダイレクト追従・netguard/robotsの結線を担う。

use crate::error::{ExitCode, Result, WebgrabError};
use crate::{netguard, robots::Robots};
use std::net::{SocketAddr, ToSocketAddrs};
use std::time::Duration;
use url::Url;

const MAX_HOPS: usize = 10;
const ROBOTS_MAX_BYTES: usize = 512 * 1024;

/// 対応メディアタイプか（大小無視）。空は許可（Content-Type欠落サーバ向け）。
fn is_supported_media_type(main: &str) -> bool {
    let m = main.to_ascii_lowercase();
    m.is_empty() || m == "text/html" || m == "application/xhtml+xml" || m == "text/plain"
}

/// 取得結果。
pub struct Fetched {
    pub final_url: String,
    pub content_type: Option<String>,
    pub body: Vec<u8>,
}

/// オプション（fetch層が必要とする分だけ）。
pub struct FetchOptions {
    pub user_agent: String,
    pub timeout: Duration,
    pub max_bytes: u64,
    pub allow_private: bool,
    pub check_robots: bool,
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
    let host_owned = host.to_string();
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
            if netguard::is_internal(a.ip()) {
                return Err(
                    WebgrabError::new(ExitCode::Netguard, "refused internal address").with_detail(
                        format!(
                            "host={host} resolved={} (use --allow-private to override)",
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
            let Some(loc) = resp
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
            else {
                return Ok(true);
            };
            let Ok(next) = Url::parse(&target).and_then(|b| b.join(loc)) else {
                return Ok(true);
            };
            if !netguard::is_allowed_scheme(next.scheme()) {
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

/// 対象URL単体のrobots.txt許可判定（render経路用）。静的経路と同じ範囲＝トップURLのみを
/// 確認する。ホスト解決とnetguard検証も行う。`--no-robots`相当のスキップは呼び出し側の責務。
pub async fn robots_precheck(url_str: &str, opts: &FetchOptions) -> Result<bool> {
    let url = Url::parse(url_str).map_err(|e| {
        WebgrabError::new(ExitCode::Usage, "invalid URL").with_detail(e.to_string())
    })?;
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
    robots_allowed(&url, addr, opts).await
}

/// 静的取得を手動リダイレクト追従で行う（各ホップでnetguard再適用）。
pub async fn fetch(url_str: &str, opts: &FetchOptions) -> Result<Fetched> {
    let mut current = Url::parse(url_str).map_err(|e| {
        WebgrabError::new(ExitCode::Usage, "invalid URL").with_detail(e.to_string())
    })?;

    for hop in 0..MAX_HOPS {
        if !netguard::is_allowed_scheme(current.scheme()) {
            return Err(WebgrabError::new(
                ExitCode::Usage,
                format!("unsupported scheme: {}", current.scheme()),
            ));
        }
        let host = current
            .host_str()
            .ok_or_else(|| WebgrabError::new(ExitCode::Usage, "URL has no host"))?
            .to_string();
        let port = current.port_or_known_default().unwrap_or(80);
        let addr = resolve_checked(&host, port, opts.allow_private, opts.timeout).await?;

        // robots確認（各ホップの着地ホストに対して、解決済みaddrを再利用）
        if opts.check_robots && !robots_allowed(&current, addr, opts).await? {
            return Err(WebgrabError::new(ExitCode::Robots, "blocked by robots.txt")
                .with_detail(format!("url={current}")));
        }

        let client = reqwest::Client::builder()
            .user_agent(&opts.user_agent)
            .timeout(opts.timeout)
            .redirect(reqwest::redirect::Policy::none())
            .resolve(&host, addr)
            .build()
            .map_err(|e| {
                WebgrabError::new(ExitCode::Internal, "client build failed")
                    .with_detail(e.to_string())
            })?;

        let resp = client.get(current.as_str()).send().await.map_err(|e| {
            WebgrabError::new(ExitCode::Network, "request failed").with_detail(e.to_string())
        })?;

        let status = resp.status();
        if status.is_redirection() {
            let location = resp
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| WebgrabError::new(ExitCode::Network, "redirect without location"))?;
            current = current.join(location).map_err(|e| {
                WebgrabError::new(ExitCode::Network, "invalid redirect location")
                    .with_detail(e.to_string())
            })?;
            if hop == MAX_HOPS - 1 {
                return Err(WebgrabError::new(ExitCode::Network, "too many redirects"));
            }
            continue;
        }

        if status.is_client_error() || status.is_server_error() {
            let retryable = status.is_server_error() || status.as_u16() == 429;
            return Err(WebgrabError::new(
                ExitCode::Http,
                format!("HTTP {} retryable={}", status.as_u16(), retryable),
            ));
        }

        let content_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());

        // 非HTML判定（メディアタイプは大小無視: RFC 9110 §8.3.1）
        if let Some(ct) = &content_type {
            let main = ct.split(';').next().unwrap_or("").trim();
            if !is_supported_media_type(main) {
                return Err(WebgrabError::new(
                    ExitCode::Http,
                    format!("unsupported content-type: {main}"),
                ));
            }
        }

        // ストリーミング読み込みで max_bytes を展開後サイズに適用
        let body = read_capped(resp, opts.max_bytes).await?;
        return Ok(Fetched {
            final_url: current.to_string(),
            content_type,
            body,
        });
    }
    Err(WebgrabError::new(ExitCode::Network, "too many redirects"))
}

/// robots.txt をチャンク単位で読み、上限超過・読み取り失敗時は None を返す。
/// 上限超過は「大きすぎる robots は許可扱い」というポリシーに合わせて呼び出し側で処理する。
async fn read_capped_robots(mut resp: reqwest::Response, max_bytes: usize) -> Option<Vec<u8>> {
    let mut buf: Vec<u8> = Vec::new();
    while let Some(chunk) = resp.chunk().await.ok()? {
        if buf.len() + chunk.len() > max_bytes {
            return None; // 上限超過
        }
        buf.extend_from_slice(&chunk);
    }
    Some(buf)
}

async fn read_capped(mut resp: reqwest::Response, max_bytes: u64) -> Result<Vec<u8>> {
    // chunk()は逐次読み込みで、展開後（gzip等デコード後）のバイト列を返す。
    let mut buf: Vec<u8> = Vec::new();
    while let Some(chunk) = resp.chunk().await.map_err(|e| {
        WebgrabError::new(ExitCode::Network, "stream read failed").with_detail(e.to_string())
    })? {
        if buf.len() as u64 + chunk.len() as u64 > max_bytes {
            return Err(WebgrabError::new(
                ExitCode::Http,
                format!("response exceeds --max-bytes ({max_bytes})"),
            ));
        }
        buf.extend_from_slice(&chunk);
    }
    Ok(buf)
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

    #[tokio::test]
    async fn resolve_allows_localhost_with_flag() {
        // allow_private=true なら通る（解決自体は成功する前提のためエラーでもコード違い）
        let r = resolve_checked("localhost", 80, true, Duration::from_secs(5)).await;
        assert!(r.is_ok() || r.unwrap_err().code == ExitCode::Network);
    }
}
