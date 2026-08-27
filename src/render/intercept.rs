//! CDP Fetch interception（第一層のSSRF遮断）とNetwork監視（設計 08 §4.2 手順1）。
//!
//! `Fetch.requestPaused`をイベントごとの子タスクへ渡し、宛先ホストをnetguardで判定して
//! 内部アドレス宛を`Fetch.failRequest`で遮断する（fail-closed）。あわせて`Network`の
//! 4イベントを購読し、`in_flight`集合と展開後バイトの計上を行う。

use super::wait::{self, DecodedBudget, InFlight};
use crate::error::{ExitCode, WebgrabError};
use crate::netguard;
use crate::renderproxy::{HostCache, Resolution};
use chromiumoxide::cdp::browser_protocol::fetch::{
    ContinueRequestParams, EnableParams, EventRequestPaused, FailRequestParams,
};
use chromiumoxide::cdp::browser_protocol::network::{
    ErrorReason, EventDataReceived, EventLoadingFailed, EventLoadingFinished,
    EventRequestWillBeSent,
};
use chromiumoxide::cdp::browser_protocol::page::FrameId;
use chromiumoxide::page::Page;
use futures::{Stream, StreamExt};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::task::JoinHandle;
use url::Url;

const INTERCEPT_CONCURRENCY: usize = 16;
const SYNC_WAIT_MAX: Duration = Duration::from_millis(500);

/// poisonを無視してロックする。interceptの子タスクがpanicしても、
/// finalize側がpoison由来のpanicで巻き添えにならないようにする。
pub(super) fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// drive/監視/interceptが共有する状態。
pub(super) struct Shared {
    pub(super) main_blocked: AtomicBool,
    /// 遮断したメインナビゲーションのホストと解決結果（終了コード8の詳細行用）。
    pub(super) blocked_main: Mutex<Option<(String, Resolution)>>,
    pub(super) inflight: Mutex<InFlight>,
    pub(super) decoded: DecodedBudget,
    pub(super) blocked_intercept: AtomicU64,
    pub(super) received: AtomicU64,
    pub(super) processed: AtomicU64,
    pub(super) cache: Arc<HostCache>,
    pub(super) allow_private: bool,
    pub(super) main_frame: FrameId,
}

/// interceptが受け取ったイベントの処理完了を最大500ms待つ（設計§4.2 手順6）。
pub(super) async fn sync_wait(shared: &Shared) {
    let start = Instant::now();
    while start.elapsed() < SYNC_WAIT_MAX {
        if shared.processed.load(Ordering::SeqCst) >= shared.received.load(Ordering::SeqCst) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// 遮断件数の通知行（設計§4.2 手順7）。0件の層は行を出さない。
pub(super) fn netguard_warn_lines(intercept: u64, proxy: u64) -> Vec<String> {
    let mut v = Vec::new();
    if intercept > 0 {
        v.push(format!(
            "webgrab: warn=netguard-blocked layer=intercept count={intercept}"
        ));
    }
    if proxy > 0 {
        v.push(format!(
            "webgrab: warn=netguard-blocked layer=proxy count={proxy}"
        ));
    }
    v
}

/// 終了コード8の詳細行（04-design.md §7: 解決IPと対象レンジを含める）。
pub(super) fn netguard_detail(
    blocked: Option<&(String, Resolution)>,
    intercept: u64,
    proxy: u64,
) -> String {
    let tail = format!("(intercept={intercept} proxy={proxy}; use --allow-private to override)");
    match blocked {
        Some((host, Resolution::Denied { ip, range })) => {
            format!("layer=intercept host={host} resolved={ip} range={range} {tail}")
        }
        Some((host, Resolution::Unresolved)) => {
            format!("layer=intercept host={host} resolved=unresolved {tail}")
        }
        Some((host, Resolution::Timeout)) => {
            format!("layer=intercept host={host} resolved=timeout {tail}")
        }
        // 記録が無い（intercept側の記録より先にdriveが戻った）場合の汎用行。
        _ => format!("layer=intercept main-navigation blocked {tail}"),
    }
}

/// リクエストURLのホストを判定する（第一層）。http(s)以外はChromeに任せる。
/// 遮断するときだけホスト名と解決結果を返す（終了コード8の詳細行が使う）。
pub(super) async fn host_denial(
    cache: &HostCache,
    request_url: &str,
    allow_private: bool,
) -> Option<(String, Resolution)> {
    let u = Url::parse(request_url).ok()?;
    // file:はローカルファイルの読み出しに使えるため、http(s)以外で唯一fail-closedで遮断する。
    // --allow-privateは内部「アドレス」の許可であり、ローカルファイルの持ち出しは含まない。
    if u.scheme() == "file" {
        return Some((
            u.host_str().unwrap_or("file").to_string(),
            Resolution::Unresolved,
        ));
    }
    if allow_private {
        return None;
    }
    if !netguard::is_allowed_scheme(u.scheme()) {
        return None;
    }
    let host = u.host_str()?;
    let port = u.port_or_known_default().unwrap_or(80);
    match cache.resolve_checked(host, port).await {
        Resolution::Allowed(_) => None,
        r => Some((host.to_string(), r)),
    }
}

/// 共有状態を作り、`Fetch.enable`と`Network`購読を済ませて監視・interceptタスクを起動する
/// （設計§4.2 手順1）。戻り値のハンドルは呼び出し側がabort-on-dropガードで保持する。
pub(super) async fn install(
    page: &Page,
    opts: &super::RenderOptions,
    cache: Arc<HostCache>,
    main_frame: FrameId,
    mirror: Arc<AtomicBool>,
) -> Result<(Arc<Shared>, JoinHandle<()>, JoinHandle<()>), WebgrabError> {
    let err = |m: &'static str, e: String| WebgrabError::new(ExitCode::Render, m).with_detail(e);
    let shared = Arc::new(Shared {
        main_blocked: AtomicBool::new(false),
        blocked_main: Mutex::new(None),
        inflight: Mutex::new(InFlight::new()),
        decoded: DecodedBudget::new(opts.max_bytes),
        blocked_intercept: AtomicU64::new(0),
        received: AtomicU64::new(0),
        processed: AtomicU64::new(0),
        cache,
        allow_private: opts.allow_private,
        main_frame,
    });

    page.execute(EnableParams::default())
        .await
        .map_err(|e| err("fetch enable failed", e.to_string()))?;

    let sent = page
        .event_listener::<EventRequestWillBeSent>()
        .await
        .map_err(|e| err("listener failed", e.to_string()))?;
    let fin = page
        .event_listener::<EventLoadingFinished>()
        .await
        .map_err(|e| err("listener failed", e.to_string()))?;
    let fail = page
        .event_listener::<EventLoadingFailed>()
        .await
        .map_err(|e| err("listener failed", e.to_string()))?;
    let data = page
        .event_listener::<EventDataReceived>()
        .await
        .map_err(|e| err("listener failed", e.to_string()))?;
    let monitor = spawn_monitor(shared.clone(), sent, fin, fail, data);

    let paused = page
        .event_listener::<EventRequestPaused>()
        .await
        .map_err(|e| err("listener failed", e.to_string()))?;
    let intercept = spawn_intercept(page.clone(), paused, shared.clone(), mirror);

    Ok((shared, monitor, intercept))
}

/// `Network`の4イベントを購読して`in_flight`と展開後バイトを更新するタスクを起動する。
fn spawn_monitor<A, B, C, D>(
    sh: Arc<Shared>,
    mut sent: A,
    mut fin: B,
    mut fail: C,
    mut data: D,
) -> JoinHandle<()>
where
    A: Stream<Item = Arc<EventRequestWillBeSent>> + Unpin + Send + 'static,
    B: Stream<Item = Arc<EventLoadingFinished>> + Unpin + Send + 'static,
    C: Stream<Item = Arc<EventLoadingFailed>> + Unpin + Send + 'static,
    D: Stream<Item = Arc<EventDataReceived>> + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        loop {
            tokio::select! {
                Some(ev) = sent.next() => {
                    lock(&sh.inflight).on_request(ev.request_id.inner(), ev.redirect_response.is_some(), Instant::now());
                }
                Some(ev) = fin.next() => { lock(&sh.inflight).on_done(ev.request_id.inner(), Instant::now()); }
                Some(ev) = fail.next() => { lock(&sh.inflight).on_done(ev.request_id.inner(), Instant::now()); }
                Some(ev) = data.next() => { sh.decoded.on_data(ev.data_length.max(0) as u64); }
                else => break,
            }
        }
    })
}

/// `Fetch.requestPaused`を個別タスク（同時16）で処理するタスクを起動する。
fn spawn_intercept<P>(
    page: Page,
    mut paused: P,
    sh: Arc<Shared>,
    mirror: Arc<AtomicBool>,
) -> JoinHandle<()>
where
    P: Stream<Item = Arc<EventRequestPaused>> + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let sem = Arc::new(tokio::sync::Semaphore::new(INTERCEPT_CONCURRENCY));
        // 子タスクはJoinSetが所有する。tokio::spawnで切り離すと、親（このタスク）が
        // abort-on-dropガードで落ちても子は生き残り、Chrome終了後のCDP発行が残る。
        // JoinSetはdropで全メンバをabortするため、親のabortが子まで届く。
        let mut set = tokio::task::JoinSet::new();
        while let Some(ev) = paused.next().await {
            sh.received.fetch_add(1, Ordering::SeqCst);
            let permit = sem.clone().acquire_owned().await;
            let (page, sh, mirror) = (page.clone(), sh.clone(), mirror.clone());
            set.spawn(async move {
                let _permit = permit;
                handle_paused(page, ev, sh, mirror).await;
            });
            // 完了済みを回収してJoinSetの要素数を有界に保つ。
            while set.try_join_next().is_some() {}
        }
    })
}

async fn handle_paused(
    page: Page,
    ev: Arc<EventRequestPaused>,
    sh: Arc<Shared>,
    mirror: Arc<AtomicBool>,
) {
    let deny = host_denial(&sh.cache, &ev.request.url, sh.allow_private).await;
    if let Some((host, res)) = deny {
        sh.blocked_intercept.fetch_add(1, Ordering::SeqCst);
        if wait::is_main_navigation(&ev.resource_type, &ev.frame_id, &sh.main_frame) {
            *lock(&sh.blocked_main) = Some((host, res));
            sh.main_blocked.store(true, Ordering::SeqCst);
            mirror.store(true, Ordering::SeqCst);
        }
        if let Some(nid) = &ev.network_id {
            lock(&sh.inflight).on_done(nid.inner(), Instant::now());
        }
        match FailRequestParams::builder()
            .request_id(ev.request_id.clone())
            .error_reason(ErrorReason::AccessDenied)
            .build()
        {
            Ok(p) => {
                let _ = page.execute(p).await;
            }
            // 遮断パラメータを組めなくても要求を握り潰さない。継続させても
            // プロキシ層（第二層）が同じ判定で拒否するため漏洩しない。
            Err(_) => {
                eprintln!("webgrab: warn=intercept-build-failed");
                let _ = page
                    .execute(ContinueRequestParams::new(ev.request_id.clone()))
                    .await;
            }
        }
    } else {
        match ContinueRequestParams::builder()
            .request_id(ev.request_id.clone())
            .build()
        {
            Ok(p) => {
                let _ = page.execute(p).await;
            }
            Err(_) => {
                eprintln!("webgrab: warn=intercept-build-failed");
                let _ = page
                    .execute(ContinueRequestParams::new(ev.request_id.clone()))
                    .await;
            }
        }
    }
    sh.processed.fetch_add(1, Ordering::SeqCst);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lock_tolerates_poisoned_mutex() {
        let m = Arc::new(Mutex::new(1u32));
        let m2 = m.clone();
        let _ = std::thread::spawn(move || {
            let _g = m2.lock().unwrap();
            panic!("poison");
        })
        .join();
        assert!(
            m.lock().is_err(),
            "mutexがpoisonされていない前提が崩れている"
        );
        assert_eq!(*lock(&m), 1);
    }

    #[test]
    fn intercept_fallback_continue_params_carry_only_request_id() {
        // build()失敗時の再試行は request_id だけを載せる（他フィールドを補わない）。
        let p = ContinueRequestParams::new("req-1".to_string());
        assert_eq!(p.request_id.inner(), "req-1");
        assert!(p.url.is_none());
        assert!(p.method.is_none());
        assert!(p.post_data.is_none());
        assert!(p.headers.is_none());
        assert!(p.intercept_response.is_none());
    }

    #[test]
    fn netguard_warn_lines_only_for_nonzero_layers() {
        assert!(netguard_warn_lines(0, 0).is_empty());
        assert_eq!(
            netguard_warn_lines(2, 0),
            vec!["webgrab: warn=netguard-blocked layer=intercept count=2"]
        );
        assert_eq!(
            netguard_warn_lines(1, 3),
            vec![
                "webgrab: warn=netguard-blocked layer=intercept count=1",
                "webgrab: warn=netguard-blocked layer=proxy count=3",
            ]
        );
    }

    #[test]
    fn netguard_detail_carries_ip_and_range() {
        let denied = (
            "meta.test".to_string(),
            Resolution::Denied {
                ip: "169.254.169.254".parse().unwrap(),
                range: "link-local",
            },
        );
        assert_eq!(
            netguard_detail(Some(&denied), 2, 1),
            "layer=intercept host=meta.test resolved=169.254.169.254 range=link-local (intercept=2 proxy=1; use --allow-private to override)"
        );
        let unres = ("x.invalid".to_string(), Resolution::Unresolved);
        assert_eq!(
            netguard_detail(Some(&unres), 1, 0),
            "layer=intercept host=x.invalid resolved=unresolved (intercept=1 proxy=0; use --allow-private to override)"
        );
        let to = ("slow.test".to_string(), Resolution::Timeout);
        assert_eq!(
            netguard_detail(Some(&to), 1, 0),
            "layer=intercept host=slow.test resolved=timeout (intercept=1 proxy=0; use --allow-private to override)"
        );
        assert_eq!(
            netguard_detail(None, 0, 0),
            "layer=intercept main-navigation blocked (intercept=0 proxy=0; use --allow-private to override)"
        );
    }

    #[tokio::test]
    async fn allow_private_short_circuits() {
        // allow_private=true では常に「内部でない」を返す（明示的オプトアウト）。
        assert!(
            host_denial(&HostCache::new(true), "http://127.0.0.1/", true)
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn unresolvable_host_is_fail_closed() {
        // .invalid は名前解決できない（RFC 6761）。fail-closedで遮断されること（A10）。
        assert!(
            host_denial(&HostCache::new(false), "http://nonexistent.invalid/", false)
                .await
                .is_some()
        );
    }

    #[tokio::test]
    async fn non_http_scheme_is_passed_through() {
        // data:等はネットワーク解決対象でなくChromeに委ねる（遮断しない）。
        assert!(
            host_denial(&HostCache::new(false), "data:text/html,hi", false)
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn file_scheme_is_denied_fail_closed() {
        // file:はローカルファイル読み出しに使えるため遮断する。--allow-privateでも解除しない。
        assert!(
            host_denial(&HostCache::new(false), "file:///etc/passwd", false)
                .await
                .is_some()
        );
        assert!(
            host_denial(&HostCache::new(true), "file:///etc/passwd", true)
                .await
                .is_some()
        );
        // ネットワークを経ないスキームはChromeに委ねる（遮断しない）。
        for u in ["blob:https://a.test/1234", "about:blank"] {
            assert!(
                host_denial(&HostCache::new(false), u, false)
                    .await
                    .is_none(),
                "{u}"
            );
        }
    }

    #[tokio::test]
    async fn literal_internal_ip_denied_in_render() {
        // ホストがIPリテラルで内部レンジなら解決成功→遮断（A10）。
        assert!(
            host_denial(
                &HostCache::new(false),
                "http://169.254.169.254/latest/meta-data/",
                false
            )
            .await
            .is_some()
        );
        assert!(
            host_denial(&HostCache::new(false), "http://[::1]/", false)
                .await
                .is_some()
        );
    }
}
