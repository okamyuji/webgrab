//! JSレンダリング（設計§3.1 render経路, §4 render、chromiumoxide + CDP Fetch interception）。
//!
//! Chromeが取得する全リクエスト（メイン+サブリソース）をFetchドメインで横取りし、
//! 宛先ホストをnetguardで判定して内部アドレス宛を遮断する。

use crate::error::{ExitCode, Result, WebgrabError};
use crate::netguard;
use chromiumoxide::browser::{Browser, BrowserConfig};
use chromiumoxide::cdp::browser_protocol::fetch::{
    ContinueRequestParams, EnableParams, EventRequestPaused, FailRequestParams,
};
use chromiumoxide::cdp::browser_protocol::network::ErrorReason;
use futures::StreamExt;
use std::sync::Arc;
use std::time::Duration;
use url::Url;

pub struct RenderOptions {
    pub timeout: Duration,
    pub wait_ms: u64,
    pub allow_private: bool,
    pub chrome_path: Option<String>,
}

/// URLをChromeでレンダリングし、安定後のDOM HTMLを返す。
pub async fn render(url_str: &str, opts: &RenderOptions) -> Result<String> {
    let fut = render_inner(url_str, opts);
    match tokio::time::timeout(opts.timeout, fut).await {
        Ok(r) => r,
        Err(_) => Err(WebgrabError::new(
            ExitCode::Render,
            "render timed out (--timeout exceeded)",
        )),
    }
}

async fn render_inner(url_str: &str, opts: &RenderOptions) -> Result<String> {
    // 一時user-data-dirを生成する。TempDirのDrop（RAII）でディレクトリが削除されるため、
    // --timeoutキャンセルやpanic時もプロファイルが残置されない（設計§4）。
    let user_data = tempfile::Builder::new()
        .prefix("webgrab-chrome-")
        .tempdir()
        .map_err(|e| {
            WebgrabError::new(ExitCode::Render, "temp dir failed").with_detail(e.to_string())
        })?;

    let mut builder = BrowserConfig::builder()
        .new_headless_mode()
        .user_data_dir(user_data.path());
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
    let handler_task = tokio::spawn(async move { while handler.next().await.is_some() {} });

    let result = drive(&mut browser, url_str, opts).await;

    let _ = browser.close().await;
    let _ = handler_task.await;
    result
}

async fn drive(browser: &mut Browser, url_str: &str, opts: &RenderOptions) -> Result<String> {
    let page = browser.new_page("about:blank").await.map_err(|e| {
        WebgrabError::new(ExitCode::Render, "new page failed").with_detail(e.to_string())
    })?;

    // Fetchドメインを有効化しrequest interceptionを開始する。
    page.execute(EnableParams::default()).await.map_err(|e| {
        WebgrabError::new(ExitCode::Render, "fetch enable failed").with_detail(e.to_string())
    })?;

    let allow_private = opts.allow_private;
    let main_blocked = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let main_blocked_cl = main_blocked.clone();

    let mut paused = page
        .event_listener::<EventRequestPaused>()
        .await
        .map_err(|e| {
            WebgrabError::new(ExitCode::Render, "listener failed").with_detail(e.to_string())
        })?;
    let page_for_intercept = page.clone();
    let intercept = tokio::spawn(async move {
        while let Some(ev) = paused.next().await {
            let deny = host_is_internal(&ev.request.url, allow_private).await;
            if deny {
                if ev.resource_type
                    == chromiumoxide::cdp::browser_protocol::network::ResourceType::Document
                {
                    main_blocked_cl.store(true, std::sync::atomic::Ordering::SeqCst);
                }
                // request_idは常に設定されるためbuild()は成功する。失敗時はstderrに警告して継続。
                match FailRequestParams::builder()
                    .request_id(ev.request_id.clone())
                    .error_reason(ErrorReason::AccessDenied)
                    .build()
                {
                    Ok(params) => {
                        let _ = page_for_intercept.execute(params).await;
                    }
                    Err(e) => eprintln!("webgrab: warn=intercept fail-request build error: {e}"),
                }
            } else {
                match ContinueRequestParams::builder()
                    .request_id(ev.request_id.clone())
                    .build()
                {
                    Ok(params) => {
                        let _ = page_for_intercept.execute(params).await;
                    }
                    Err(e) => eprintln!("webgrab: warn=intercept continue build error: {e}"),
                }
            }
        }
    });

    let nav = page.goto(url_str).await;
    if main_blocked.load(std::sync::atomic::Ordering::SeqCst) {
        intercept.abort();
        return Err(WebgrabError::new(
            ExitCode::Netguard,
            "refused internal address during render",
        ));
    }
    nav.map_err(|e| {
        WebgrabError::new(ExitCode::Render, "navigation failed").with_detail(e.to_string())
    })?;

    tokio::time::sleep(Duration::from_millis(opts.wait_ms)).await;
    let content = page.content().await.map_err(|e| {
        WebgrabError::new(ExitCode::Render, "content read failed").with_detail(e.to_string())
    })?;

    intercept.abort();
    Ok(content)
}

/// リクエストURLのホストを解決し内部アドレスか判定する。解決失敗は遮断しない。
async fn host_is_internal(request_url: &str, allow_private: bool) -> bool {
    if allow_private {
        return false;
    }
    let Ok(u) = Url::parse(request_url) else {
        return false;
    };
    if !netguard::is_allowed_scheme(u.scheme()) {
        // data:等はChromeに任せる（サブリソースのdata:は無害）
        return false;
    }
    let Some(host) = u.host_str().map(|s| s.to_string()) else {
        return false;
    };
    let port = u.port_or_known_default().unwrap_or(80);
    tokio::task::spawn_blocking(move || {
        use std::net::ToSocketAddrs;
        match (host.as_str(), port).to_socket_addrs() {
            Ok(addrs) => addrs.into_iter().any(|a| netguard::is_internal(a.ip())),
            Err(_) => false,
        }
    })
    .await
    .unwrap_or(false)
}
