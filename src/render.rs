//! JSレンダリング（設計 08 §4.2）。chromiumoxide + CDP Fetch interception + Network監視。
//!
//! SSRFは二層で防ぐ。第一層はCDP Fetchドメインでページセッションの全リクエストを横取りし、
//! 宛先ホストをnetguardで判定して内部アドレス宛を遮断する（fail-closed）。第二層は
//! [`renderproxy`]の検証・IPピン留めプロキシで、Chromeの全接続（OOPIF/Service Workerを含む）を
//! 経由させ、判定と接続のIP一致を保証してDNSリバインディング(TOCTOU)を閉じる。
//! `--max-bytes`は`Network.dataReceived`の展開後バイト（ページセッション）と、
//! `content()`前のDOM長評価で有界にする。

pub mod wait;

mod intercept;
mod world;

use crate::error::{ExitCode, Result, WebgrabError};
use crate::renderproxy::{self, HostCache, ProxyState, Resolution};
use chromiumoxide::browser::{Browser, BrowserConfig};
use futures::StreamExt;
use intercept::{Shared, install, lock, netguard_detail, netguard_warn_lines, sync_wait};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use wait::{effective_cap, exceed_msg};
use world::{IsolatedWorld, eval_limit};

pub struct RenderOptions {
    pub timeout: Duration,
    pub wait_ms: u64,
    pub allow_private: bool,
    pub chrome_path: Option<String>,
    /// 残余の展開後バイト上限（超過は終了コード4）。
    pub max_bytes: u64,
    /// 利用者指定の--max-bytes（メッセージ表示用）。
    pub max_bytes_total: u64,
    pub no_sandbox: bool,
}

const NAV_WAIT_MAX: Duration = Duration::from_millis(1000);

/// Dropでabortするタスクガード。早期リターン・--timeoutキャンセルでもタスクを残置しない。
struct AbortOnDrop(tokio::task::JoinHandle<()>);
impl AbortOnDrop {
    /// ガードを保ったままタスクの終了を待つ。待機が早期リターンで中断されても
    /// Dropがabortするため、タスクは残置されない。
    async fn join(&mut self) {
        let _ = (&mut self.0).await;
    }
}
impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Chrome起動フラグ（プロキシ強制 + loopbackバイパス無効化）を返す。
/// chromiumoxideのArgsBuilderが各キーへ先頭`--`を付与するため、ここでは`--`を付けない。
/// `--`を付けると`----key`となりChromeが無視し、プロキシが不活性化する（回帰防止）。
fn proxy_args(port: u16) -> [String; 2] {
    [
        format!("proxy-server=127.0.0.1:{port}"),
        "proxy-bypass-list=<-loopback>".to_string(),
    ]
}

/// 終了コード8を単一経路で判定する（設計§4.2 手順7）。driveの結果によらず先にmain_blockedを見る。
#[allow(clippy::too_many_arguments)]
fn finalize(
    main_blocked: &AtomicBool,
    blocked_main: Option<&(String, Resolution)>,
    result: Result<String>,
    blocked_intercept: u64,
    blocked_proxy: u64,
    proxy_exceeded: bool,
    proxy_bytes: u64,
    max_bytes_total: u64,
) -> Result<String> {
    // 終了コード8の経路でも遮断件数は通知する（SSRF試行を無通知にしない）。
    for l in netguard_warn_lines(blocked_intercept, blocked_proxy) {
        eprintln!("{l}");
    }
    if main_blocked.load(Ordering::SeqCst) {
        return Err(WebgrabError::new(
            ExitCode::Netguard,
            "refused internal address during render",
        )
        .with_detail(netguard_detail(
            blocked_main,
            blocked_intercept,
            blocked_proxy,
        )));
    }
    // プロキシ層はChromeの全接続（メイン以外を含む）を通すため、Fetch intercept層が見ない
    // 超過（例: 単一の大きなクロスオリジンiframe）もここで終了コード4に写像する。
    if proxy_exceeded {
        return Err(WebgrabError::new(
            ExitCode::Http,
            exceed_msg(proxy_bytes, max_bytes_total, ""),
        ));
    }
    result
}

/// URLをChromeでレンダリングし、安定後のDOM HTMLを返す。
pub async fn render(url_str: &str, opts: &RenderOptions) -> Result<String> {
    let deadline = Instant::now() + opts.timeout;
    match tokio::time::timeout(opts.timeout, render_inner(url_str, opts, deadline)).await {
        Ok(r) => r,
        Err(_) => Err(WebgrabError::new(
            ExitCode::Render,
            "render timed out (--timeout exceeded)",
        )),
    }
}

async fn render_inner(url_str: &str, opts: &RenderOptions, deadline: Instant) -> Result<String> {
    // 一時user-data-dirを生成する。TempDirのDrop（RAII）でディレクトリが削除されるため、
    // --timeoutキャンセルやpanic時もプロファイルが残置されない（設計§4）。
    let user_data = tempfile::Builder::new()
        .prefix("webgrab-chrome-")
        .tempdir()
        .map_err(|e| {
            WebgrabError::new(ExitCode::Render, "temp dir failed").with_detail(e.to_string())
        })?;
    let cache = Arc::new(HostCache::new(opts.allow_private));
    let (proxy_addr, proxy_state, proxy_handle) = renderproxy::spawn(cache.clone(), opts.max_bytes)
        .await
        .map_err(|e| {
            WebgrabError::new(ExitCode::Render, "ssrf proxy start failed")
                .with_detail(e.to_string())
        })?;
    let _proxy_guard = AbortOnDrop(proxy_handle);

    let mut builder = BrowserConfig::builder()
        .new_headless_mode()
        .user_data_dir(user_data.path())
        .args(proxy_args(proxy_addr.port()));
    if opts.no_sandbox {
        builder = builder.no_sandbox();
    }
    if let Some(p) = &opts.chrome_path {
        builder = builder.chrome_executable(p);
    }
    let config = builder
        .build()
        .map_err(|e| WebgrabError::new(ExitCode::Render, "chrome config failed").with_detail(e))?;
    let (mut browser, mut handler) = Browser::launch(config).await.map_err(|e| {
        WebgrabError::new(
            ExitCode::Render,
            "chrome launch failed (is Chrome installed?)",
        )
        .with_detail(e.to_string())
    })?;
    let mut handler_task = AbortOnDrop(tokio::spawn(async move {
        while handler.next().await.is_some() {}
    }));

    let main_blocked = Arc::new(AtomicBool::new(false));
    let (result, blocked_intercept, blocked_main) = drive(
        &mut browser,
        url_str,
        opts,
        deadline,
        cache,
        &proxy_state,
        main_blocked.clone(),
    )
    .await;

    let _ = browser.close().await;
    handler_task.join().await;

    finalize(
        &main_blocked,
        blocked_main.as_ref(),
        result,
        blocked_intercept,
        proxy_state.denied(),
        proxy_state.exceeded(),
        proxy_state.downloaded(),
        opts.max_bytes_total,
    )
}

async fn drive(
    browser: &mut Browser,
    url_str: &str,
    opts: &RenderOptions,
    deadline: Instant,
    cache: Arc<HostCache>,
    proxy_state: &ProxyState,
    main_blocked: Arc<AtomicBool>,
) -> (Result<String>, u64, Option<(String, Resolution)>) {
    let shared_holder: Arc<Mutex<Option<Arc<Shared>>>> = Arc::new(Mutex::new(None));
    let r = drive_inner(
        browser,
        url_str,
        opts,
        deadline,
        cache,
        proxy_state,
        main_blocked,
        shared_holder.clone(),
    )
    .await;
    let held = lock(&shared_holder).clone();
    let blocked = held
        .as_ref()
        .map(|s| s.blocked_intercept.load(Ordering::SeqCst))
        .unwrap_or(0);
    let blocked_main = held.as_ref().and_then(|s| lock(&s.blocked_main).clone());
    (r, blocked, blocked_main)
}

#[allow(clippy::too_many_arguments)]
async fn drive_inner(
    browser: &mut Browser,
    url_str: &str,
    opts: &RenderOptions,
    deadline: Instant,
    cache: Arc<HostCache>,
    proxy_state: &ProxyState,
    main_blocked: Arc<AtomicBool>,
    shared_holder: Arc<Mutex<Option<Arc<Shared>>>>,
) -> Result<String> {
    let render_err =
        |m: &'static str, e: String| WebgrabError::new(ExitCode::Render, m).with_detail(e);
    let page = browser
        .new_page("about:blank")
        .await
        .map_err(|e| render_err("new page failed", e.to_string()))?;

    // 手順0: メインフレームID（Fetch.enable前に取得。取れなければfail-closedで終了コード7）
    let main_frame = page
        .mainframe()
        .await
        .map_err(|e| render_err("main frame id unavailable", e.to_string()))?
        .ok_or_else(|| WebgrabError::new(ExitCode::Render, "main frame id unavailable"))?;

    // 手順1: 共有状態・監視タスク・interceptタスクの設置。
    // main_blockedは外側(finalize)が読むArcへ転写するため、Shared側の変化を都度反映する。
    let (shared, monitor, intercept) =
        install(&page, opts, cache, main_frame.clone(), main_blocked).await?;
    *lock(&shared_holder) = Some(shared.clone());
    let _monitor = AbortOnDrop(monitor);
    let _intercept = AbortOnDrop(intercept);

    let blocked_now = |sh: &Shared| sh.main_blocked.load(Ordering::SeqCst);
    let netguard_err =
        || WebgrabError::new(ExitCode::Netguard, "refused internal address during render");
    // Nは実測値そのもの（network=受信済み展開後バイト、dom=DOM長）。
    let exceed_err = |n: u64, what: &str| {
        WebgrabError::new(ExitCode::Http, exceed_msg(n, opts.max_bytes_total, what))
    };

    // 手順2: goto（失敗時も同期待ち+再確認してから8/7を決める）
    let t_goto = Instant::now();
    let cap = effective_cap(opts.wait_ms, deadline, t_goto);
    if let Err(e) = page.goto(url_str).await {
        sync_wait(&shared).await;
        if blocked_now(&shared) {
            return Err(netguard_err());
        }
        return Err(render_err("navigation failed", e.to_string()));
    }
    let nav_wait = NAV_WAIT_MAX.min(deadline.saturating_duration_since(Instant::now()));
    let _ = tokio::time::timeout(nav_wait, page.wait_for_navigation()).await; // 最善努力

    // 分離ワールド（ページ側の上書きが効かない文脈で評価する）。goto後に遅延生成する（about:blankの文脈は破棄済み）。
    let mut world = IsolatedWorld {
        page: page.clone(),
        frame: main_frame.clone(),
        ctx: None,
    };

    // 手順3〜5: ポーリング
    let mut prev: Option<[u64; 2]> = None;
    let mut stable: u32 = 0;
    loop {
        if blocked_now(&shared) {
            return Err(netguard_err());
        }
        if shared.decoded.exceeded() {
            return Err(exceed_err(shared.decoded.total(), "network"));
        }
        let idle = lock(&shared.inflight).is_idle(Instant::now());
        let limit = eval_limit(
            deadline.saturating_duration_since(Instant::now()),
            cap.saturating_sub(t_goto.elapsed()),
        );
        match world.measure(limit).await {
            Some(cur) => {
                if prev == Some(cur) {
                    stable += 1
                } else {
                    stable = 0
                }
                prev = Some(cur);
            }
            None => stable = 0,
        }
        let text_len = prev.map(|c| c[1] as usize).unwrap_or(0);
        if wait::should_stop(idle, stable, text_len, t_goto.elapsed(), cap) {
            break;
        }
        let left = cap.saturating_sub(t_goto.elapsed());
        tokio::time::sleep(Duration::from_millis(wait::POLL_MS).min(left)).await;
    }

    // 手順6: 同期待ち → 再確認 → DOM長 → content()
    sync_wait(&shared).await;
    if blocked_now(&shared) {
        return Err(netguard_err());
    }
    if shared.decoded.exceeded() {
        return Err(exceed_err(shared.decoded.total(), "network"));
    }
    let limit = eval_limit(
        deadline.saturating_duration_since(Instant::now()),
        cap.saturating_sub(t_goto.elapsed()),
    );
    if let Some(dom_len) = world.dom_length(limit).await {
        let budget_left = opts.max_bytes.saturating_sub(shared.decoded.total());
        if dom_len > budget_left {
            return Err(exceed_err(dom_len, "dom"));
        }
    }
    let limit = eval_limit(
        deadline.saturating_duration_since(Instant::now()),
        cap.saturating_sub(t_goto.elapsed()),
    );
    // page.content()はメインワールド評価でgetter上書きに弱いため、分離ワールドで取得する。
    let content = world
        .dom_html(limit)
        .await
        .ok_or_else(|| WebgrabError::new(ExitCode::Render, "content read failed or timed out"))?;
    if blocked_now(&shared) {
        return Err(netguard_err());
    }
    let _ = proxy_state; // 超過判定はrender_innerがfinalizeへ渡す（exceeded()/downloaded()を使用）
    Ok(content)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    #[test]
    fn proxy_args_have_no_leading_dashes() {
        // chromiumoxideが`--`を前置するため、キーに`--`を付けてはならない。
        // 付けると`----proxy-server`になりChromeが無視しプロキシが無効化する。
        let args = proxy_args(12345);
        assert_eq!(args[0], "proxy-server=127.0.0.1:12345");
        assert_eq!(args[1], "proxy-bypass-list=<-loopback>");
        for a in &args {
            assert!(!a.starts_with('-'), "先頭に`-`があってはならない: {a}");
        }
    }

    #[test]
    fn exit8_takes_precedence_over_any_drive_result() {
        let blocked = Arc::new(AtomicBool::new(true));
        let r = finalize(
            &blocked,
            None,
            Err(WebgrabError::new(ExitCode::Http, "x")),
            0,
            0,
            false,
            0,
            0,
        );
        assert_eq!(r.unwrap_err().code, ExitCode::Netguard);
        let r2 = finalize(
            &blocked,
            None,
            Ok("<html></html>".into()),
            0,
            0,
            false,
            0,
            0,
        );
        assert_eq!(r2.unwrap_err().code, ExitCode::Netguard);
        let clear = Arc::new(AtomicBool::new(false));
        assert!(finalize(&clear, None, Ok("<html></html>".into()), 0, 0, false, 0, 0).is_ok());
    }

    #[test]
    fn proxy_exceeded_maps_to_http_exit_when_not_blocked() {
        let clear = Arc::new(AtomicBool::new(false));
        let r = finalize(
            &clear,
            None,
            Ok("<html></html>".into()),
            0,
            0,
            true,
            12_000,
            10_000,
        );
        let e = r.unwrap_err();
        assert_eq!(e.code, ExitCode::Http);
        assert!(
            e.message.starts_with(
                "render download exceeds remaining --max-bytes budget (12000 of 10000)"
            )
        );
    }

    #[test]
    fn main_blocked_takes_precedence_over_proxy_exceeded() {
        let blocked = Arc::new(AtomicBool::new(true));
        let r = finalize(
            &blocked,
            None,
            Ok("<html></html>".into()),
            0,
            0,
            true,
            12_000,
            10_000,
        );
        assert_eq!(r.unwrap_err().code, ExitCode::Netguard);
    }

    #[tokio::test]
    async fn abort_on_drop_join_then_drop_leaves_no_task() {
        let done = Arc::new(AtomicBool::new(false));
        let d = done.clone();
        let mut g = AbortOnDrop(tokio::spawn(async move {
            d.store(true, Ordering::SeqCst);
        }));
        g.join().await;
        assert!(done.load(Ordering::SeqCst));
        assert!(g.0.is_finished());

        // 未完了タスクはdropでabortされる。
        let g2 = AbortOnDrop(tokio::spawn(std::future::pending::<()>()));
        let raw = g2.0.abort_handle();
        drop(g2);
        tokio::task::yield_now().await;
        assert!(raw.is_finished());
    }
}
