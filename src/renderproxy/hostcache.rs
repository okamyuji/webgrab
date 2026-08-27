//! ホスト解決とnetguard判定を1本のコールドミス解決に畳むキャッシュ（設計§4.2）。
//!
//! intercept層とプロキシ層で共有する`HostCache`本体と、その内部キャッシュ構造を持つ。

use crate::netguard;
use std::collections::{HashMap, VecDeque};
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

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
}
