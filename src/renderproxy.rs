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
use std::collections::{HashMap, VecDeque};
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const MAX_HEAD_BYTES: usize = 64 * 1024;
const RELAY_BUF: usize = 16 * 1024;
const RESOLVE_TIMEOUT: Duration = Duration::from_secs(2);

/// ホスト解決とnetguard判定の結果。終了コード8の詳細行が解決IPとレンジを要求するため
/// 「拒否した理由」まで持つ（04-design.md §7 / 08-js-render-design.md §5）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolution {
    Allowed(SocketAddr),
    Denied {
        ip: IpAddr,
        range: &'static str,
    },
    /// 名前解決自体が失敗した、または解決結果が空。
    Unresolved,
    /// 2秒の解決上限を超えた。
    Timeout,
}

/// キャッシュ項目。同一キーの同時コールドミスを1回の解決に畳む。
enum Entry {
    Ready(Resolution),
    Pending(tokio::sync::watch::Receiver<Option<Resolution>>),
}

/// キャッシュ上限。悪意あるページが無数のサブドメインを要求してもメモリを有界に保つ。
const MAX_CACHED_HOSTS: usize = 4096;

/// キャッシュ本体。挿入順を`order`に持ち、上限超過で最古のキーから捨てる。
#[derive(Default)]
struct Cache {
    map: HashMap<(String, u16), Entry>,
    order: VecDeque<(String, u16)>,
}

impl Cache {
    /// 上限を保ったまま挿入する。新規キーのときだけ挿入順に積む
    /// （Pending→Readyの差し替えで同じキーを二重に積まない）。
    fn insert_bounded(&mut self, key: (String, u16), entry: Entry) {
        if self.map.insert(key.clone(), entry).is_none() {
            self.order.push_back(key);
        }
        while self.map.len() > MAX_CACHED_HOSTS {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            self.map.remove(&oldest);
        }
    }
}

/// ホスト解決の実行単位キャッシュ。intercept層とプロキシ層で共有する（設計§4.2）。
pub struct HostCache {
    allow_private: bool,
    map: tokio::sync::Mutex<Cache>,
}

impl HostCache {
    pub fn new(allow_private: bool) -> Self {
        Self {
            allow_private,
            map: tokio::sync::Mutex::new(Cache::default()),
        }
    }

    /// 解決→netguard判定→検証済みIP。2秒上限、失敗・超過・内部はNone（fail-closed）。結果はキャッシュ。
    pub async fn resolve(&self, host: &str, port: u16) -> Option<SocketAddr> {
        match self.resolve_checked(host, port).await {
            Resolution::Allowed(a) => Some(a),
            _ => None,
        }
    }

    /// `resolve`と同じ判定を行い、拒否時はIPとレンジまで返す。
    pub async fn resolve_checked(&self, host: &str, port: u16) -> Resolution {
        let key = (host.to_ascii_lowercase(), port);
        // 解決はロックの外で行う。ロックを跨いで持つと同時ミスが直列化する。
        let tx = {
            let mut c = self.map.lock().await;
            match c.map.get(&key) {
                Some(Entry::Ready(r)) => return *r,
                Some(Entry::Pending(rx)) => {
                    let mut rx = rx.clone();
                    drop(c);
                    // 先着の2秒上限に相乗りする。送信側が消えた場合もfail-closed。
                    return match tokio::time::timeout(RESOLVE_TIMEOUT, rx.changed()).await {
                        Ok(Ok(())) => (*rx.borrow()).unwrap_or(Resolution::Timeout),
                        _ => Resolution::Timeout,
                    };
                }
                None => {
                    let (tx, rx) = tokio::sync::watch::channel(None);
                    c.insert_bounded(key.clone(), Entry::Pending(rx));
                    tx
                }
            }
        };

        let h = host.to_string();
        let allow_private = self.allow_private;
        let task = tokio::task::spawn_blocking(move || {
            use std::net::ToSocketAddrs;
            let Ok(it) = (h.as_str(), port).to_socket_addrs() else {
                return Resolution::Unresolved;
            };
            let addrs: Vec<SocketAddr> = it.collect();
            let Some(first) = addrs.first().copied() else {
                return Resolution::Unresolved;
            };
            if !allow_private {
                for a in &addrs {
                    if let Some(range) = netguard::deny_range(a.ip()) {
                        return Resolution::Denied { ip: a.ip(), range };
                    }
                }
            }
            Resolution::Allowed(first)
        });
        // 上限超過はfail-closed（遮断）。spawn_blockingのスレッドは回収されないが実行単位で有界。
        let v = match tokio::time::timeout(RESOLVE_TIMEOUT, task).await {
            Ok(Ok(v)) => v,
            Ok(Err(_)) => Resolution::Unresolved,
            Err(_) => Resolution::Timeout,
        };
        self.map.lock().await.insert_bounded(key, Entry::Ready(v));
        let _ = tx.send(Some(v));
        v
    }

    #[cfg(test)]
    pub async fn cached_len(&self) -> usize {
        self.map.lock().await.map.len()
    }
}

/// プロキシの共有状態。ダウンロード総量と上限超過フラグ、遮断件数を全接続で共有する。
pub struct ProxyState {
    cache: Arc<HostCache>,
    max_bytes: u64,
    downloaded: AtomicU64,
    exceeded: AtomicBool,
    denied: AtomicU64,
}

impl ProxyState {
    /// `--max-bytes`のダウンロード総量上限を超えたか。
    pub fn exceeded(&self) -> bool {
        self.exceeded.load(Ordering::SeqCst)
    }

    /// netguard判定で遮断した接続数。
    pub fn denied(&self) -> u64 {
        self.denied.load(Ordering::SeqCst)
    }

    /// これまでにプロキシ経由で転送した総バイト数（超過メッセージの表示用）。
    pub fn downloaded(&self) -> u64 {
        self.downloaded.load(Ordering::SeqCst)
    }
}

/// プロキシを127.0.0.1の空きポートで起動し、待受アドレス・共有状態・タスクハンドルを返す。
pub async fn spawn(
    cache: Arc<HostCache>,
    max_bytes: u64,
) -> std::io::Result<(SocketAddr, Arc<ProxyState>, tokio::task::JoinHandle<()>)> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
    let addr = listener.local_addr()?;
    let state = Arc::new(ProxyState {
        cache,
        max_bytes,
        downloaded: AtomicU64::new(0),
        exceeded: AtomicBool::new(false),
        denied: AtomicU64::new(0),
    });
    let st = state.clone();
    let handle = tokio::spawn(async move {
        loop {
            // accept()の一時エラー（ECONNABORTED、EMFILE等）でループを抜けてはならない。
            // 抜けるとChromeの以後の接続がすべて拒否され、サブリソースを欠いたDOMを
            // 終了コード0で返してしまう。少し待って受付を続ける（busy loopも避ける）。
            match listener.accept().await {
                Ok((stream, _)) => {
                    let st = st.clone();
                    tokio::spawn(async move {
                        let _ = handle_conn(stream, st).await;
                    });
                }
                Err(_) => {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    continue;
                }
            }
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
    let Some(addr) = st.cache.resolve(&host, port).await else {
        st.denied.fetch_add(1, Ordering::SeqCst);
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
    let Some(addr) = st.cache.resolve(&host, port).await else {
        st.denied.fetch_add(1, Ordering::SeqCst);
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
    async fn host_cache_resolves_once_and_denies_internal() {
        let c = HostCache::new(false);
        assert!(c.resolve("127.0.0.1", 80).await.is_none(), "内部は遮断");
        assert!(
            c.resolve("nonexistent.invalid", 80).await.is_none(),
            "解決不能はfail-closed"
        );
        let c3 = HostCache::new(false);
        assert!(
            c3.resolve("::1", 80).await.is_none(),
            "allow_private=false ではIPv6ループバックも遮断"
        );
        let c2 = HostCache::new(true);
        let a = c2.resolve("127.0.0.1", 80).await;
        assert_eq!(a.map(|s| s.port()), Some(80));
        assert_eq!(c2.cached_len().await, 1);
        let _ = c2.resolve("127.0.0.1", 80).await;
        assert_eq!(c2.cached_len().await, 1, "2回目はキャッシュ");
    }

    #[tokio::test]
    async fn concurrent_cold_misses_resolve_once() {
        // 同一キーの同時コールドミスは1回の解決に畳まれ、全員が同じ結果を得る。
        let c = Arc::new(HostCache::new(true));
        let futs = (0..8).map(|_| {
            let c = c.clone();
            async move { c.resolve("127.0.0.1", 80).await }
        });
        let rs = futures::future::join_all(futs).await;
        assert!(rs.iter().all(|r| r.is_some()), "{rs:?}");
        assert_eq!(c.cached_len().await, 1);
    }

    #[tokio::test]
    async fn resolve_checked_reports_range_for_denied() {
        let c = HostCache::new(false);
        match c.resolve_checked("127.0.0.1", 80).await {
            Resolution::Denied { ip, range } => {
                assert_eq!(ip.to_string(), "127.0.0.1");
                assert_eq!(range, "loopback");
            }
            other => panic!("expected Denied, got {other:?}"),
        }
        assert!(matches!(
            c.resolve_checked("nonexistent.invalid", 80).await,
            Resolution::Unresolved
        ));
    }

    #[tokio::test]
    async fn proxy_counts_denials() {
        let (addr, st, _h) = spawn(Arc::new(HostCache::new(false)), 1_000_000)
            .await
            .unwrap();
        let mut c = TcpStream::connect(addr).await.unwrap();
        c.write_all(b"CONNECT 127.0.0.1:9 HTTP/1.1\r\nHost: 127.0.0.1:9\r\n\r\n")
            .await
            .unwrap();
        let mut sink = Vec::new();
        let _ = c.read_to_end(&mut sink).await;
        assert!(String::from_utf8_lossy(&sink).starts_with("HTTP/1.1 403"));
        assert_eq!(st.denied(), 1);
    }

    #[tokio::test]
    async fn proxy_denies_connect_to_metadata_endpoint() {
        let (addr, _st, _h) = spawn(Arc::new(HostCache::new(false)), NO_CAP)
            .await
            .unwrap();
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

        let (addr, st, _h) = spawn(Arc::new(HostCache::new(true)), 1024).await.unwrap();
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
    async fn accept_loop_survives_an_aborted_client_handshake() {
        // ヘッダ未完了で切断するクライアントの後も受付が続くこと（I1の回帰）。
        let (addr, st, _h) = spawn(Arc::new(HostCache::new(false)), NO_CAP)
            .await
            .unwrap();
        let aborted = TcpStream::connect(addr).await.unwrap();
        drop(aborted);
        for _ in 0..2 {
            let mut c = TcpStream::connect(addr).await.unwrap();
            c.write_all(b"CONNECT 127.0.0.1:9 HTTP/1.1\r\nHost: 127.0.0.1:9\r\n\r\n")
                .await
                .unwrap();
            let mut sink = Vec::new();
            let _ = c.read_to_end(&mut sink).await;
            assert!(
                String::from_utf8_lossy(&sink).starts_with("HTTP/1.1 403"),
                "中断後の接続が処理されていない"
            );
        }
        assert_eq!(st.denied(), 2);
    }

    #[tokio::test]
    async fn host_cache_is_bounded() {
        // 上限を1件超える異なるキーを入れても、最古から捨てられて上限内に収まる。
        let c = HostCache::new(true);
        for port in 1..=(MAX_CACHED_HOSTS as u16 + 1) {
            let _ = c.resolve("127.0.0.1", port).await;
        }
        assert!(
            c.cached_len().await <= MAX_CACHED_HOSTS,
            "上限を超えた: {}",
            c.cached_len().await
        );
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
        let (addr, st, _h) = spawn(Arc::new(HostCache::new(true)), 1_000_000)
            .await
            .unwrap();
        let mut c = TcpStream::connect(addr).await.unwrap();
        c.write_all(format!("CONNECT {up_addr} HTTP/1.1\r\nHost: {up_addr}\r\n\r\n").as_bytes())
            .await
            .unwrap();
        let mut sink = Vec::new();
        let _ = c.read_to_end(&mut sink).await;
        assert!(!st.exceeded(), "上限内なのに超過フラグが立った");
    }
}
