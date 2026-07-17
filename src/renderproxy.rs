//! render経路のSSRF完全防御用の検証・IPピン留めローカルプロキシ（設計§3.1）。
//!
//! Chromeの全リクエスト（メイン+サブリソース、loopback含む）をこのプロキシへ強制し、
//! ホストを解決→netguard検証（fail-closed）→検証したIPへ接続を固定する。
//! Chrome自身にDNS解決・接続をさせないため、判定と接続でIPが食い違うDNSリバインディング
//! (TOCTOU)が原理的に発生しない。fetch.rsのIPピン留めと同じ保証をrender経路へ与える。
//!
//! あわせて全接続の集約点でダウンロード総量を計上し、`--max-bytes`超過を検出する
//! （render経路のDoS対策、設計§3.1の総ダウンロード量上限）。

use crate::netguard;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const MAX_HEAD_BYTES: usize = 64 * 1024;
const RELAY_BUF: usize = 16 * 1024;

/// プロキシの共有状態。ダウンロード総量と上限超過フラグを全接続で共有する。
pub struct ProxyState {
    allow_private: bool,
    max_bytes: u64,
    downloaded: AtomicU64,
    exceeded: AtomicBool,
}

impl ProxyState {
    /// `--max-bytes`のダウンロード総量上限を超えたか。
    pub fn exceeded(&self) -> bool {
        self.exceeded.load(Ordering::SeqCst)
    }
}

/// プロキシを127.0.0.1の空きポートで起動し、待受アドレス・共有状態・タスクハンドルを返す。
pub async fn spawn(
    allow_private: bool,
    max_bytes: u64,
) -> std::io::Result<(SocketAddr, Arc<ProxyState>, tokio::task::JoinHandle<()>)> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
    let addr = listener.local_addr()?;
    let state = Arc::new(ProxyState {
        allow_private,
        max_bytes,
        downloaded: AtomicU64::new(0),
        exceeded: AtomicBool::new(false),
    });
    let st = state.clone();
    let handle = tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let st = st.clone();
            tokio::spawn(async move {
                let _ = handle_conn(stream, st).await;
            });
        }
    });
    Ok((addr, state, handle))
}

/// `host:port` / `[ipv6]:port` / `host` を分解する。ポート省略時は`default_port`。
fn parse_host_port(authority: &str, default_port: u16) -> Option<(String, u16)> {
    let authority = authority.trim();
    if authority.is_empty() {
        return None;
    }
    if let Some(rest) = authority.strip_prefix('[') {
        // [ipv6] または [ipv6]:port
        let close = rest.find(']')?;
        let host = &rest[..close];
        let after = &rest[close + 1..];
        let port = match after.strip_prefix(':') {
            Some(p) => p.parse().ok()?,
            None => default_port,
        };
        if host.is_empty() {
            return None;
        }
        return Some((host.to_string(), port));
    }
    match authority.rsplit_once(':') {
        Some((h, p)) if !h.is_empty() && !p.is_empty() => Some((h.to_string(), p.parse().ok()?)),
        _ => Some((authority.to_string(), default_port)),
    }
}

/// `CONNECT host:port HTTP/1.1` からhost/portを取り出す。
fn parse_connect_target(line: &str) -> Option<(String, u16)> {
    let mut it = line.split_whitespace();
    if !it.next()?.eq_ignore_ascii_case("CONNECT") {
        return None;
    }
    parse_host_port(it.next()?, 443)
}

/// 絶対形式リクエスト行 `METHOD scheme://host[:port]/path HTTP/1.1` を分解し、
/// (host, port, origin形式に書き換えたリクエスト行) を返す。
fn parse_absolute_request(line: &str) -> Option<(String, u16, String)> {
    let mut it = line.split_whitespace();
    let method = it.next()?;
    let target = it.next()?;
    let version = it.next().unwrap_or("HTTP/1.1");
    let (scheme, rest) = target.split_once("://")?;
    let default_port = if scheme.eq_ignore_ascii_case("https") {
        443
    } else if scheme.eq_ignore_ascii_case("http") {
        80
    } else {
        return None; // http/https以外はプロキシしない
    };
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let (host, port) = parse_host_port(authority, default_port)?;
    let origin_line = format!("{method} {path} {version}");
    Some((host, port, origin_line))
}

/// ホストを解決し、内部アドレスでなければ接続先の検証済みIPを返す（fail-closed）。
async fn validate_and_pin(host: &str, port: u16, allow_private: bool) -> Option<SocketAddr> {
    let host = host.to_string();
    tokio::task::spawn_blocking(move || {
        use std::net::ToSocketAddrs;
        let addrs: Vec<SocketAddr> = (host.as_str(), port).to_socket_addrs().ok()?.collect();
        if addrs.is_empty() {
            return None;
        }
        if !allow_private && addrs.iter().any(|a| netguard::is_internal(a.ip())) {
            return None; // 内部アドレスを含む → 遮断
        }
        Some(addrs[0]) // 検証済みIPへピン留め
    })
    .await
    .ok()
    .flatten()
}

async fn handle_conn(mut client: TcpStream, st: Arc<ProxyState>) -> std::io::Result<()> {
    // リクエストヘッダ末尾（\r\n\r\n）まで読む。
    let mut head: Vec<u8> = Vec::new();
    let mut buf = [0u8; 8192];
    let sep = loop {
        if let Some(pos) = find_subslice(&head, b"\r\n\r\n") {
            break pos;
        }
        if head.len() > MAX_HEAD_BYTES {
            let _ = client
                .write_all(b"HTTP/1.1 431 Request Header Fields Too Large\r\n\r\n")
                .await;
            return Ok(());
        }
        let n = client.read(&mut buf).await?;
        if n == 0 {
            return Ok(()); // ヘッダ未完了で切断
        }
        head.extend_from_slice(&buf[..n]);
    };

    let first_line_end = find_subslice(&head, b"\r\n").unwrap_or(head.len());
    let first_line = String::from_utf8_lossy(&head[..first_line_end]).to_string();

    if first_line
        .get(..7)
        .is_some_and(|s| s.eq_ignore_ascii_case("CONNECT"))
    {
        handle_connect(client, &first_line, st).await
    } else {
        // ヘッダ以降に既に読み込んだボディ断片
        let leftover = head[sep + 4..].to_vec();
        handle_http(client, &head[..sep + 4], &first_line, leftover, st).await
    }
}

async fn handle_connect(
    mut client: TcpStream,
    first_line: &str,
    st: Arc<ProxyState>,
) -> std::io::Result<()> {
    let Some((host, port)) = parse_connect_target(first_line) else {
        let _ = client.write_all(b"HTTP/1.1 400 Bad Request\r\n\r\n").await;
        return Ok(());
    };
    let Some(addr) = validate_and_pin(&host, port, st.allow_private).await else {
        let _ = client
            .write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n")
            .await;
        return Ok(());
    };
    let upstream = match TcpStream::connect(addr).await {
        Ok(s) => s,
        Err(_) => {
            let _ = client.write_all(b"HTTP/1.1 502 Bad Gateway\r\n\r\n").await;
            return Ok(());
        }
    };
    client
        .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
        .await?;
    // 以降はTLSの生バイトをピン留め先IPへ双方向転送する（内容は覗かない）。
    relay_capped(client, upstream, st).await;
    Ok(())
}

async fn handle_http(
    mut client: TcpStream,
    head: &[u8],
    first_line: &str,
    leftover: Vec<u8>,
    st: Arc<ProxyState>,
) -> std::io::Result<()> {
    let Some((host, port, origin_line)) = parse_absolute_request(first_line) else {
        let _ = client.write_all(b"HTTP/1.1 400 Bad Request\r\n\r\n").await;
        return Ok(());
    };
    let Some(addr) = validate_and_pin(&host, port, st.allow_private).await else {
        let _ = client
            .write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n")
            .await;
        return Ok(());
    };
    let mut upstream = match TcpStream::connect(addr).await {
        Ok(s) => s,
        Err(_) => {
            let _ = client.write_all(b"HTTP/1.1 502 Bad Gateway\r\n\r\n").await;
            return Ok(());
        }
    };
    // リクエスト行をorigin形式へ書き換え、Connection: closeを強制して
    // 1接続=1リクエストにする（keep-aliveで別ホストが混ざるのを防ぐ）。
    let rewritten = rewrite_head(head, first_line, &origin_line);
    upstream.write_all(rewritten.as_bytes()).await?;
    if !leftover.is_empty() {
        upstream.write_all(&leftover).await?;
    }
    relay_capped(client, upstream, st).await;
    Ok(())
}

/// client↔upstreamを双方向転送し、upstream→client（=ダウンロード）方向のバイト数を
/// 共有カウンタへ加算する。総量が`max_bytes`を超えたら`exceeded`を立てて転送を打ち切る。
async fn relay_capped(mut client: TcpStream, mut upstream: TcpStream, st: Arc<ProxyState>) {
    let (mut cr, mut cw) = client.split();
    let (mut ur, mut uw) = upstream.split();

    // アップロード方向（client→upstream）は計上しない。
    let upload = async {
        let _ = tokio::io::copy(&mut cr, &mut uw).await;
    };
    // ダウンロード方向（upstream→client）を計上しつつ転送する。
    let download = async {
        let mut buf = [0u8; RELAY_BUF];
        loop {
            let n = match ur.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            let total = st.downloaded.fetch_add(n as u64, Ordering::SeqCst) + n as u64;
            if total > st.max_bytes {
                st.exceeded.store(true, Ordering::SeqCst);
                break; // 上限超過。dropで両方向を閉じる
            }
            if cw.write_all(&buf[..n]).await.is_err() {
                break;
            }
        }
    };

    tokio::select! {
        _ = upload => {}
        _ = download => {}
    }
}

/// ヘッダブロックの先頭行をorigin形式に差し替え、プロキシ関連/keep-alive系ヘッダを除去し
/// `Connection: close` を付与する。
fn rewrite_head(head: &[u8], first_line: &str, origin_line: &str) -> String {
    let text = String::from_utf8_lossy(head);
    let mut out = String::with_capacity(text.len());
    out.push_str(origin_line);
    out.push_str("\r\n");
    for line in text.split("\r\n").skip(1) {
        if line.is_empty() {
            continue;
        }
        let lower = line.to_ascii_lowercase();
        if lower.starts_with("proxy-connection:")
            || lower.starts_with("connection:")
            || lower.starts_with("keep-alive:")
        {
            continue;
        }
        out.push_str(line);
        out.push_str("\r\n");
    }
    let _ = first_line;
    out.push_str("Connection: close\r\n\r\n");
    out
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NO_CAP: u64 = u64::MAX;

    #[test]
    fn parse_host_port_variants() {
        assert_eq!(
            parse_host_port("example.com:8080", 80),
            Some(("example.com".into(), 8080))
        );
        assert_eq!(
            parse_host_port("example.com", 80),
            Some(("example.com".into(), 80))
        );
        assert_eq!(parse_host_port("[::1]:443", 80), Some(("::1".into(), 443)));
        assert_eq!(
            parse_host_port("[fe80::1]", 80),
            Some(("fe80::1".into(), 80))
        );
        assert_eq!(parse_host_port("", 80), None);
    }

    #[test]
    fn parse_connect_target_ok() {
        assert_eq!(
            parse_connect_target("CONNECT example.com:443 HTTP/1.1"),
            Some(("example.com".into(), 443))
        );
        assert_eq!(
            parse_connect_target("connect 169.254.169.254:80 HTTP/1.1"),
            Some(("169.254.169.254".into(), 80))
        );
        assert_eq!(parse_connect_target("GET / HTTP/1.1"), None);
    }

    #[test]
    fn parse_absolute_request_rewrites_to_origin() {
        let (h, p, line) =
            parse_absolute_request("GET http://example.com/path?q=1 HTTP/1.1").unwrap();
        assert_eq!((h.as_str(), p), ("example.com", 80));
        assert_eq!(line, "GET /path?q=1 HTTP/1.1");
        let (h, p, line) = parse_absolute_request("POST http://a.test:8080 HTTP/1.1").unwrap();
        assert_eq!((h.as_str(), p), ("a.test", 8080));
        assert_eq!(line, "POST / HTTP/1.1");
        assert!(parse_absolute_request("GET ftp://a.test/x HTTP/1.1").is_none());
    }

    #[test]
    fn rewrite_head_forces_connection_close_and_strips_proxy_headers() {
        let head = b"GET http://a.test/ HTTP/1.1\r\nHost: a.test\r\nProxy-Connection: keep-alive\r\nConnection: keep-alive\r\n\r\n";
        let out = rewrite_head(head, "GET http://a.test/ HTTP/1.1", "GET / HTTP/1.1");
        assert!(out.starts_with("GET / HTTP/1.1\r\n"));
        assert!(out.contains("Host: a.test\r\n"));
        assert!(!out.to_ascii_lowercase().contains("proxy-connection"));
        assert!(out.contains("Connection: close\r\n"));
        assert!(!out.contains("Connection: keep-alive"));
    }

    #[tokio::test]
    async fn validate_and_pin_denies_internal_literal() {
        assert!(
            validate_and_pin("169.254.169.254", 80, false)
                .await
                .is_none()
        );
        assert!(validate_and_pin("127.0.0.1", 80, false).await.is_none());
        assert!(validate_and_pin("::1", 80, false).await.is_none());
    }

    #[tokio::test]
    async fn validate_and_pin_fail_closed_on_unresolvable() {
        assert!(
            validate_and_pin("nonexistent.invalid", 80, false)
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn validate_and_pin_allows_internal_with_flag() {
        assert!(validate_and_pin("127.0.0.1", 80, true).await.is_some());
    }

    #[tokio::test]
    async fn proxy_denies_connect_to_metadata_endpoint() {
        let (addr, _st, _h) = spawn(false, NO_CAP).await.unwrap();
        let mut c = TcpStream::connect(addr).await.unwrap();
        c.write_all(b"CONNECT 169.254.169.254:443 HTTP/1.1\r\nHost: 169.254.169.254:443\r\n\r\n")
            .await
            .unwrap();
        let mut resp = [0u8; 64];
        let n = c.read(&mut resp).await.unwrap();
        let s = String::from_utf8_lossy(&resp[..n]);
        assert!(s.contains("403"), "got: {s}");
    }

    #[tokio::test]
    async fn proxy_enforces_max_bytes_over_tunnel() {
        // ローカルの疑似upstreamを立て、CONNECTトンネル経由で上限超のデータを流すと
        // exceededが立つことを確認する（allow_private=trueで127.0.0.1を許可）。
        let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let up_addr = upstream.local_addr().unwrap();
        tokio::spawn(async move {
            if let Ok((mut s, _)) = upstream.accept().await {
                // 上限(1KiB)を大きく超える64KiBを送る
                let payload = vec![b'x'; 64 * 1024];
                let _ = s.write_all(&payload).await;
                let _ = s.shutdown().await;
            }
        });

        let (addr, st, _h) = spawn(true, 1024).await.unwrap();
        let mut c = TcpStream::connect(addr).await.unwrap();
        c.write_all(format!("CONNECT {up_addr} HTTP/1.1\r\nHost: {up_addr}\r\n\r\n").as_bytes())
            .await
            .unwrap();
        // 200 established + データを読み切る
        let mut sink = Vec::new();
        let _ = c.read_to_end(&mut sink).await;
        assert!(st.exceeded(), "max_bytes超過が検出されていない");
    }

    #[tokio::test]
    async fn proxy_under_limit_does_not_flag() {
        let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let up_addr = upstream.local_addr().unwrap();
        tokio::spawn(async move {
            if let Ok((mut s, _)) = upstream.accept().await {
                let _ = s.write_all(b"small-body").await;
                let _ = s.shutdown().await;
            }
        });
        let (addr, st, _h) = spawn(true, 1_000_000).await.unwrap();
        let mut c = TcpStream::connect(addr).await.unwrap();
        c.write_all(format!("CONNECT {up_addr} HTTP/1.1\r\nHost: {up_addr}\r\n\r\n").as_bytes())
            .await
            .unwrap();
        let mut sink = Vec::new();
        let _ = c.read_to_end(&mut sink).await;
        assert!(!st.exceeded(), "上限内なのに超過フラグが立った");
    }
}
