//! JSレンダリング（設計 08 §4.2）。chromiumoxide + CDP Fetch interception + Network監視。
//!
//! SSRFは二層で防ぐ。第一層はCDP Fetchドメインでページセッションの全リクエストを横取りし、
//! 宛先ホストをnetguardで判定して内部アドレス宛を遮断する（fail-closed）。第二層は
//! [`renderproxy`]の検証・IPピン留めプロキシで、Chromeの全接続（OOPIF/Service Workerを含む）を
//! 経由させ、判定と接続のIP一致を保証してDNSリバインディング(TOCTOU)を閉じる。
//! `--max-bytes`は`Network.dataReceived`の展開後バイト（ページセッション）と、
//! `content()`前のDOM長評価で有界にする。

pub mod wait;

use crate::error::{ExitCode, Result, WebgrabError};
use crate::netguard;
use crate::renderproxy::{self, HostCache, ProxyState, Resolution};
use chromiumoxide::browser::{Browser, BrowserConfig};
use chromiumoxide::cdp::browser_protocol::fetch::{
    ContinueRequestParams, EnableParams, EventRequestPaused, FailRequestParams,
};
use chromiumoxide::cdp::browser_protocol::network::{
    ErrorReason, EventDataReceived, EventLoadingFailed, EventLoadingFinished,
    EventRequestWillBeSent,
};
use chromiumoxide::cdp::browser_protocol::page::{CreateIsolatedWorldParams, FrameId};
use chromiumoxide::cdp::js_protocol::runtime::{EvaluateParams, ExecutionContextId};
use chromiumoxide::page::Page;
use futures::StreamExt;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use url::Url;
use wait::{DecodedBudget, InFlight};

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

const CONTENT_RESERVE: Duration = Duration::from_millis(2000);
const NAV_WAIT_MAX: Duration = Duration::from_millis(1000);
const SYNC_WAIT_MAX: Duration = Duration::from_millis(500);
const INTERCEPT_CONCURRENCY: usize = 16;

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

/// drive/監視/interceptが共有する状態。
struct Shared {
    main_blocked: AtomicBool,
    /// 遮断したメインナビゲーションのホストと解決結果（終了コード8の詳細行用）。
    blocked_main: Mutex<Option<(String, Resolution)>>,
    inflight: Mutex<InFlight>,
    decoded: DecodedBudget,
    blocked_intercept: AtomicU64,
    received: AtomicU64,
    processed: AtomicU64,
    cache: Arc<HostCache>,
    allow_private: bool,
    main_frame: FrameId,
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

/// `goto`直前に確定する実効待機上限: min(--wait-ms, deadline − now − 予備2000ms)。
fn effective_cap(wait_ms: u64, deadline: Instant, now: Instant) -> Duration {
    let remaining = deadline
        .saturating_duration_since(now)
        .saturating_sub(CONTENT_RESERVE);
    Duration::from_millis(wait_ms).min(remaining)
}

/// `--max-bytes`超過メッセージ。`what`が空なら層の接尾辞を付けない。
fn exceed_msg(n: u64, max_bytes_total: u64, what: &str) -> String {
    let base =
        format!("render download exceeds remaining --max-bytes budget ({n} of {max_bytes_total})");
    if what.is_empty() {
        base
    } else {
        format!("{base} [{what}]")
    }
}

/// 遮断件数の通知行（設計§4.2 手順7）。0件の層は行を出さない。
fn netguard_warn_lines(intercept: u64, proxy: u64) -> Vec<String> {
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
fn netguard_detail(blocked: Option<&(String, Resolution)>, intercept: u64, proxy: u64) -> String {
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
    let held = shared_holder.lock().unwrap().clone();
    let blocked = held
        .as_ref()
        .map(|s| s.blocked_intercept.load(Ordering::SeqCst))
        .unwrap_or(0);
    let blocked_main = held
        .as_ref()
        .and_then(|s| s.blocked_main.lock().unwrap().clone());
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
        main_frame: main_frame.clone(),
    });
    *shared_holder.lock().unwrap() = Some(shared.clone());
    // main_blockedは外側(finalize)が読むArcへ転写するため、Shared側の変化を都度反映する。
    let mirror = main_blocked;

    page.execute(EnableParams::default())
        .await
        .map_err(|e| render_err("fetch enable failed", e.to_string()))?;

    // 手順1: 監視タスク（Network 4イベント）
    let mut sent = page
        .event_listener::<EventRequestWillBeSent>()
        .await
        .map_err(|e| render_err("listener failed", e.to_string()))?;
    let mut fin = page
        .event_listener::<EventLoadingFinished>()
        .await
        .map_err(|e| render_err("listener failed", e.to_string()))?;
    let mut fail = page
        .event_listener::<EventLoadingFailed>()
        .await
        .map_err(|e| render_err("listener failed", e.to_string()))?;
    let mut data = page
        .event_listener::<EventDataReceived>()
        .await
        .map_err(|e| render_err("listener failed", e.to_string()))?;
    let sh = shared.clone();
    let _monitor = AbortOnDrop(tokio::spawn(async move {
        loop {
            tokio::select! {
                Some(ev) = sent.next() => {
                    sh.inflight.lock().unwrap().on_request(ev.request_id.inner(), ev.redirect_response.is_some(), Instant::now());
                }
                Some(ev) = fin.next() => { sh.inflight.lock().unwrap().on_done(ev.request_id.inner(), Instant::now()); }
                Some(ev) = fail.next() => { sh.inflight.lock().unwrap().on_done(ev.request_id.inner(), Instant::now()); }
                Some(ev) = data.next() => { sh.decoded.on_data(ev.data_length.max(0) as u64); }
                else => break,
            }
        }
    }));

    // 手順1: interceptタスク（個別タスク化、同時16、ホスト判定キャッシュ）
    let mut paused = page
        .event_listener::<EventRequestPaused>()
        .await
        .map_err(|e| render_err("listener failed", e.to_string()))?;
    let page_i = page.clone();
    let sh = shared.clone();
    let mirror_i = mirror.clone();
    let _intercept = AbortOnDrop(tokio::spawn(async move {
        let sem = Arc::new(tokio::sync::Semaphore::new(INTERCEPT_CONCURRENCY));
        while let Some(ev) = paused.next().await {
            sh.received.fetch_add(1, Ordering::SeqCst);
            let permit = sem.clone().acquire_owned().await;
            let (page, sh, mirror) = (page_i.clone(), sh.clone(), mirror_i.clone());
            tokio::spawn(async move {
                let _permit = permit;
                let deny = host_denial(&sh.cache, &ev.request.url, sh.allow_private).await;
                if let Some((host, res)) = deny {
                    sh.blocked_intercept.fetch_add(1, Ordering::SeqCst);
                    if wait::is_main_navigation(&ev.resource_type, &ev.frame_id, &sh.main_frame) {
                        *sh.blocked_main.lock().unwrap() = Some((host, res));
                        sh.main_blocked.store(true, Ordering::SeqCst);
                        mirror.store(true, Ordering::SeqCst);
                    }
                    if let Some(nid) = &ev.network_id {
                        sh.inflight
                            .lock()
                            .unwrap()
                            .on_done(nid.inner(), Instant::now());
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
            });
        }
    }));

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
        let idle = shared.inflight.lock().unwrap().is_idle(Instant::now());
        let remaining = deadline.saturating_duration_since(Instant::now());
        match world.measure(remaining).await {
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
    let remaining = deadline.saturating_duration_since(Instant::now());
    if let Some(dom_len) = world.dom_length(remaining).await {
        let budget_left = opts.max_bytes.saturating_sub(shared.decoded.total());
        if dom_len > budget_left {
            return Err(exceed_err(dom_len, "dom"));
        }
    }
    let remaining = deadline.saturating_duration_since(Instant::now());
    // page.content()はメインワールド評価でgetter上書きに弱いため、分離ワールドで取得する。
    let content = world
        .dom_html(remaining)
        .await
        .ok_or_else(|| WebgrabError::new(ExitCode::Render, "content read failed or timed out"))?;
    if blocked_now(&shared) {
        return Err(netguard_err());
    }
    let _ = proxy_state; // 超過判定はrender_innerがfinalizeへ渡す（exceeded()/downloaded()を使用）
    Ok(content)
}

/// interceptが受け取ったイベントの処理完了を最大500ms待つ（設計§4.2 手順6）。
async fn sync_wait(shared: &Shared) {
    let start = Instant::now();
    while start.elapsed() < SYNC_WAIT_MAX {
        if shared.processed.load(Ordering::SeqCst) >= shared.received.load(Ordering::SeqCst) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// 分離ワールドでの数値評価。ページ側のdefineProperty等の上書きが効かない。
struct IsolatedWorld {
    page: Page,
    frame: FrameId,
    ctx: Option<ExecutionContextId>,
}

impl IsolatedWorld {
    async fn ensure_ctx(&mut self) -> Option<ExecutionContextId> {
        if let Some(c) = self.ctx {
            return Some(c);
        }
        let r = self
            .page
            .execute(
                CreateIsolatedWorldParams::builder()
                    .frame_id(self.frame.clone())
                    .world_name("webgrab")
                    .build()
                    .ok()?,
            )
            .await
            .ok()?;
        self.ctx = Some(r.execution_context_id);
        self.ctx
    }

    /// 式を評価してu64配列で返す。失敗・タイムアウト・非数値はNone（条件未達扱い）。
    async fn eval_numbers(&mut self, expr: &str, limit: Duration) -> Option<Vec<u64>> {
        for attempt in 0..2 {
            let ctx = self.ensure_ctx().await?;
            let params = EvaluateParams::builder()
                .expression(expr)
                .context_id(ctx)
                .return_by_value(true)
                .build()
                .ok()?;
            match tokio::time::timeout(limit, self.page.execute(params)).await {
                Ok(Ok(resp)) => {
                    let v = resp.result.result.value.clone()?;
                    let arr = v.as_array()?;
                    return arr
                        .iter()
                        .map(|x| x.as_f64().map(|f| f.max(0.0) as u64))
                        .collect();
                }
                Ok(Err(_)) if attempt == 0 => {
                    self.ctx = None;
                    continue;
                } // 文脈破棄→作り直して1回だけ再試行
                _ => return None,
            }
        }
        None
    }

    async fn measure(&mut self, limit: Duration) -> Option<[u64; 2]> {
        let v = self.eval_numbers(
            "(function(){var b=document.body;return [document.getElementsByTagName('*').length,(b&&b.innerText||'').trim().length];})()",
            limit,
        ).await?;
        Some([*v.first()?, *v.get(1)?])
    }

    async fn dom_length(&mut self, limit: Duration) -> Option<u64> {
        let v = self
            .eval_numbers(
                "(function(){var d=document.documentElement;return [d?d.outerHTML.length:0];})()",
                limit,
            )
            .await?;
        v.first().copied()
    }

    /// DOM HTML（doctype + outerHTML）を分離ワールドで取得する。失敗・タイムアウトはNone。
    async fn dom_html(&mut self, limit: Duration) -> Option<String> {
        const EXPR: &str = "(function(){var s='';if(document.doctype){s=new XMLSerializer().serializeToString(document.doctype);}var d=document.documentElement;if(d){s+=d.outerHTML;}return s;})()";
        for attempt in 0..2 {
            let ctx = self.ensure_ctx().await?;
            let params = EvaluateParams::builder()
                .expression(EXPR)
                .context_id(ctx)
                .return_by_value(true)
                .build()
                .ok()?;
            match tokio::time::timeout(limit, self.page.execute(params)).await {
                Ok(Ok(resp)) => {
                    return resp
                        .result
                        .result
                        .value
                        .as_ref()?
                        .as_str()
                        .map(|s| s.to_string());
                }
                Ok(Err(_)) if attempt == 0 => {
                    self.ctx = None;
                    continue;
                }
                _ => return None,
            }
        }
        None
    }
}

/// リクエストURLのホストを判定する（第一層）。http(s)以外はChromeに任せる。
/// 遮断するときだけホスト名と解決結果を返す（終了コード8の詳細行が使う）。
async fn host_denial(
    cache: &HostCache,
    request_url: &str,
    allow_private: bool,
) -> Option<(String, Resolution)> {
    if allow_private {
        return None;
    }
    let u = Url::parse(request_url).ok()?;
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
    fn effective_cap_is_clamped_by_deadline() {
        let now = Instant::now();
        let deadline = now + Duration::from_millis(3500);
        assert_eq!(
            effective_cap(5000, deadline, now),
            Duration::from_millis(1500)
        );
        assert_eq!(
            effective_cap(1000, deadline, now),
            Duration::from_millis(1000)
        );
        assert_eq!(effective_cap(5000, now, now), Duration::ZERO);
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

    #[test]
    fn exceed_msg_uses_measured_value() {
        assert_eq!(
            exceed_msg(3_000, 1_000, "dom"),
            "render download exceeds remaining --max-bytes budget (3000 of 1000) [dom]"
        );
        assert_eq!(
            exceed_msg(12, 10, ""),
            "render download exceeds remaining --max-bytes budget (12 of 10)"
        );
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
