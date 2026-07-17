//! JSレンダリング（設計§3.1 render経路, §4 render、chromiumoxide + CDP Fetch interception）。
//!
//! SSRFは二層で防ぐ。第一層はCDP Fetchドメインで全リクエスト（メイン+サブリソース）を
//! 横取りし、宛先ホストをnetguardで判定して内部アドレス宛を遮断する（fail-closed）。
//! 第二層は[`renderproxy`]の検証・IPピン留めプロキシで、Chromeの全接続を経由させ、
//! 判定と接続のIP一致を保証してDNSリバインディング(TOCTOU)を閉じる。

use crate::error::{ExitCode, Result, WebgrabError};
use crate::{netguard, renderproxy};
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
    /// ダウンロード総量の上限（プロキシで計上、超過は終了コード4）。
    pub max_bytes: u64,
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

    // SSRF完全防御: Chromeの全DNS解決・接続を、検証・IPピン留めを行う
    // ローカルプロキシ経由に強制する。これによりChrome自身の再解決に起因する
    // DNSリバインディング(TOCTOU)を原理的に閉じる。<-loopback>でloopback宛も
    // バイパスさせず、内部アドレスへの直結を防ぐ。
    let (proxy_addr, proxy_state, proxy_handle) =
        renderproxy::spawn(opts.allow_private, opts.max_bytes)
            .await
            .map_err(|e| {
                WebgrabError::new(ExitCode::Render, "ssrf proxy start failed")
                    .with_detail(e.to_string())
            })?;

    let mut builder = BrowserConfig::builder()
        .new_headless_mode()
        .user_data_dir(user_data.path())
        .args(proxy_args(proxy_addr.port()));
    if let Some(p) = &opts.chrome_path {
        builder = builder.chrome_executable(p);
    }
    let config = match builder.build() {
        Ok(c) => c,
        Err(e) => {
            proxy_handle.abort();
            return Err(WebgrabError::new(ExitCode::Render, "chrome config failed").with_detail(e));
        }
    };

    let (mut browser, mut handler) = match Browser::launch(config).await {
        Ok(b) => b,
        Err(e) => {
            proxy_handle.abort();
            return Err(WebgrabError::new(
                ExitCode::Render,
                "chrome launch failed (is Chrome installed?)",
            )
            .with_detail(e.to_string()));
        }
    };
    let handler_task = tokio::spawn(async move { while handler.next().await.is_some() {} });

    let result = drive(&mut browser, url_str, opts).await;

    let _ = browser.close().await;
    let _ = handler_task.await;
    proxy_handle.abort(); // Chrome終了後、SSRFプロキシの待受も停止する

    // ダウンロード総量が--max-bytesを超えていたら、内容が取れていても終了コード4にする。
    if proxy_state.exceeded() {
        return Err(WebgrabError::new(
            ExitCode::Http,
            format!("render download exceeds --max-bytes ({})", opts.max_bytes),
        ));
    }
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

/// リクエストURLのホストを解決し内部アドレスか判定する（第一層）。
/// httpホストの名前解決に失敗した場合は fail-closed（＝内部扱いで遮断）とする。
/// 解決成功後にChromeが再解決して内部IPへ切り替えるDNSリバインディングは、
/// この関数単体では閉じられないが、第二層の[`renderproxy`]によるIPピン留めで塞ぐ。
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
            // 名前解決失敗は遮断側に倒す（fail-closed）。fetch.rsのresolve_checkedと対称。
            Err(_) => true,
        }
    })
    .await
    // spawn_blockingのjoin失敗も遮断側に倒す。
    .unwrap_or(true)
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[tokio::test]
    async fn allow_private_short_circuits() {
        // allow_private=true では常に「内部でない」を返す（明示的オプトアウト）。
        assert!(!host_is_internal("http://127.0.0.1/", true).await);
    }

    #[tokio::test]
    async fn unresolvable_host_is_fail_closed() {
        // .invalid は名前解決できない（RFC 6761）。fail-closedで遮断されること（A10）。
        assert!(host_is_internal("http://nonexistent.invalid/", false).await);
    }

    #[tokio::test]
    async fn non_http_scheme_is_passed_through() {
        // data:等はネットワーク解決対象でなくChromeに委ねる（遮断しない）。
        assert!(!host_is_internal("data:text/html,hi", false).await);
    }

    #[tokio::test]
    async fn literal_internal_ip_denied_in_render() {
        // ホストがIPリテラルで内部レンジなら解決成功→遮断（A10）。
        assert!(host_is_internal("http://169.254.169.254/latest/meta-data/", false).await);
        assert!(host_is_internal("http://[::1]/", false).await);
    }
}
