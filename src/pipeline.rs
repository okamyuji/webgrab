//! パイプライン結線（設計 08 §4.3）。静的フェーズ → エスカレーション判定 → renderフェーズ → 出力。

use crate::cli::{Cli, DEFAULT_WAIT_MS, FormatArg};
use crate::error::{ExitCode, Result, WebgrabError};
use crate::fetch::{self, FetchOptions};
use crate::output::{self, Format, Meta, RenderStatus};
use crate::render::{self, RenderOptions};
use crate::{budget, cli, convert, decode, extract, tokens};
use std::time::{Duration, Instant};

const SHORT_CONTENT_CHARS: usize = 200;
const SKIP_TIMEOUT_MIN: Duration = Duration::from_secs(5);
const SKIP_BYTES_MIN: u64 = 256 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Render,
    Extract,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    Timeout,
    MaxBytes,
}

impl SkipReason {
    pub fn token(self) -> &'static str {
        match self {
            SkipReason::Timeout => "timeout",
            SkipReason::MaxBytes => "max-bytes",
        }
    }
}

pub fn escalation_reason(visible_chars: usize) -> Option<&'static str> {
    if visible_chars == 0 {
        Some("empty")
    } else if visible_chars < SHORT_CONTENT_CHARS {
        Some("short")
    } else {
        None
    }
}

pub fn remaining_budget(
    timeout: Duration,
    elapsed: Duration,
    max_bytes: u64,
    consumed: u64,
) -> std::result::Result<(Duration, u64), SkipReason> {
    let rt = timeout.saturating_sub(elapsed);
    if rt < SKIP_TIMEOUT_MIN {
        return Err(SkipReason::Timeout);
    }
    let rb = max_bytes.saturating_sub(consumed);
    if rb < SKIP_BYTES_MIN {
        return Err(SkipReason::MaxBytes);
    }
    Ok((rt, rb))
}

pub fn choose_result(static_chars: usize, rendered_chars: usize) -> RenderStatus {
    if rendered_chars > static_chars {
        RenderStatus::Rendered
    } else {
        RenderStatus::NoGain
    }
}

pub fn fallback_reason(phase: Phase, err: &WebgrabError) -> Option<&'static str> {
    match phase {
        Phase::Render => match err.code {
            ExitCode::Render => Some("render"),
            ExitCode::Http => Some("max-bytes"),
            _ => None,
        },
        Phase::Extract => Some("extract"),
    }
}

/// (stderrトークン, 本文用散文)。設計§4.1: static/skippedは両方提案、renderしたなら--rawのみ。
pub fn hint_for(status: RenderStatus) -> (&'static str, &'static str) {
    match status {
        RenderStatus::Static | RenderStatus::Skipped(_) => ("--render/--raw", "--render or --raw"),
        RenderStatus::Rendered | RenderStatus::Failed(_) | RenderStatus::NoGain => {
            ("--raw", "--raw")
        }
    }
}

fn to_format(f: FormatArg) -> Format {
    match f {
        FormatArg::Markdown => Format::Markdown,
        FormatArg::Frontmatter => Format::Frontmatter,
        FormatArg::Json => Format::Json,
        FormatArg::Text => Format::Text,
        FormatArg::Html => Format::Html,
    }
}

/// 本文の作り方。終了コード6・short-content・エスカレーションの免除条件が経路ごとに違う。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Route {
    Extracted,
    Raw,
    Plain,
}

/// 抽出・変換済みの中間結果。
struct Stage {
    title: Option<String>,
    published: Option<String>,
    body: String,
    visible: usize,
    route: Route,
}

fn build_stage(cli: &Cli, html: &str, final_url: &str) -> Result<Stage> {
    let (title, published, body_html) = if cli.raw {
        (None, None, convert::strip_non_content(html))
    } else {
        let ex = extract::extract(html, final_url)?;
        (ex.title, ex.published_time, ex.content_html)
    };
    let body = match to_format(cli.format) {
        Format::Html => body_html.clone(),
        Format::Text => convert::to_text(&body_html)?,
        _ => convert::to_markdown(&body_html)?,
    };
    let visible = convert::visible_text_len(&body_html);
    Ok(Stage {
        title,
        published,
        body,
        visible,
        route: if cli.raw {
            Route::Raw
        } else {
            Route::Extracted
        },
    })
}

/// text/plainの素通し（設計10 §4.1）。抽出もMarkdown変換も行わず、危険リンクスキームの
/// 無害化だけを掛ける。抽出HTMLが無いため`visible`は本文そのものの文字数になる。
fn plain_stage(text: &str) -> Stage {
    let body = convert::sanitize_link_schemes(text);
    Stage {
        title: None,
        published: None,
        visible: body.chars().count(),
        body,
        route: Route::Plain,
    }
}

/// Chromeを実際に起動する直前にだけ出す。エスカレーションしない実行では出さない。
fn warn_no_sandbox(cli: &Cli) {
    if cli.no_sandbox {
        eprintln!("webgrab: warn=no-sandbox");
    }
}

/// `warn=auto-render-failed`の2行目。detailが無いときに末尾の空白を残さない。
fn failure_detail(e: &WebgrabError) -> String {
    crate::error::sanitize_detail(
        format!("{} {}", e.message, e.detail.as_deref().unwrap_or("")).trim_end(),
    )
}

fn render_options(cli: &Cli, timeout: Duration, max_bytes: u64) -> RenderOptions {
    RenderOptions {
        timeout,
        wait_ms: cli.wait_ms.unwrap_or(DEFAULT_WAIT_MS),
        allow_private: cli.allow_private,
        chrome_path: cli.chrome_path.clone(),
        max_bytes,
        max_bytes_total: cli.max_bytes,
        no_sandbox: cli.no_sandbox,
    }
}

/// 本文を得たフェーズの成果。エスカレーションで差し替わりうる。
struct Acquired {
    stage: Stage,
    final_url: String,
    /// 静的フェーズが消費した展開後バイト数（残余予算の計算に使う）。
    consumed: u64,
}

/// 可視テキスト長の記録（設計§4.4）。
struct CharCounts {
    static_chars: Option<usize>,
    rendered_chars: Option<usize>,
}

/// エスカレーションの結果。
struct Escalation {
    status: RenderStatus,
    rendered_chars: Option<usize>,
}

fn fetch_options(cli: &Cli, ua: String, timeout: Duration, check_robots: bool) -> FetchOptions {
    FetchOptions {
        user_agent: ua,
        timeout,
        max_bytes: cli.max_bytes,
        allow_private: cli.allow_private,
        check_robots,
    }
}

/// 1. 静的フェーズ。Content-Typeが`text/plain`なら素通し経路に入る（設計10 §4.1）。
async fn static_phase(cli: &Cli, ua: String, timeout: Duration) -> Result<Acquired> {
    let fopts = fetch_options(cli, ua, timeout, !cli.no_robots);
    let fetched = fetch::fetch(&cli.url, &fopts).await?;
    let ct = fetched.content_type.as_deref();
    let plain = fetch::is_plain_text(ct);
    let (text, enc, had_errors) = decode::decode(&fetched.body, ct, !plain);
    if had_errors {
        eprintln!("webgrab: warn=decode-replacement enc={enc}");
    }
    let stage = if plain {
        plain_stage(&text)
    } else {
        build_stage(cli, &text, &fetched.final_url)?
    };
    Ok(Acquired {
        stage,
        final_url: fetched.final_url,
        consumed: fetched.consumed_bytes,
    })
}

/// 1'. `--render`明示のフェーズ。静的取得は行わないため消費バイトは0。
async fn render_phase(cli: &Cli, ua: String, timeout: Duration) -> Result<Acquired> {
    if !cli.no_robots {
        let fopts = fetch_options(cli, ua, timeout, true);
        if !fetch::robots_precheck(&cli.url, &fopts).await? {
            return Err(WebgrabError::new(ExitCode::Robots, "blocked by robots.txt")
                .with_detail(format!("url={}", cli.url)));
        }
    }
    warn_no_sandbox(cli);
    let r = render::render(&cli.url, &render_options(cli, timeout, cli.max_bytes)).await?;
    let stage = build_stage(cli, &r.html, &r.final_url)?;
    Ok(Acquired {
        stage,
        final_url: r.final_url,
        consumed: 0,
    })
}

/// renderフェーズの失敗をwarnへ落とす。落とせないエラーはそのまま返す。
fn escalation_failed(phase: Phase, e: WebgrabError) -> Result<Escalation> {
    match fallback_reason(phase, &e) {
        Some(r) => {
            eprintln!("webgrab: warn=auto-render-failed reason={r}");
            eprintln!("{}", failure_detail(&e));
            Ok(Escalation {
                status: RenderStatus::Failed(r),
                rendered_chars: None,
            })
        }
        None => Err(e),
    }
}

/// 2〜5. エスカレーション。採用したときだけ`acq`の本文とURLを差し替える。
async fn escalate(
    cli: &Cli,
    acq: &mut Acquired,
    timeout: Duration,
    elapsed: Duration,
    reason: &'static str,
) -> Result<Escalation> {
    let (rt, rb) = match remaining_budget(timeout, elapsed, cli.max_bytes, acq.consumed) {
        Err(skip) => {
            eprintln!("webgrab: warn=auto-render-skipped reason={}", skip.token());
            return Ok(Escalation {
                status: RenderStatus::Skipped(skip.token()),
                rendered_chars: None,
            });
        }
        Ok(v) => v,
    };
    eprintln!(
        "webgrab: info=auto-render reason={reason} chars={}",
        acq.stage.visible
    );
    warn_no_sandbox(cli);
    let rendered = match render::render(&acq.final_url, &render_options(cli, rt, rb)).await {
        Ok(r) => r,
        Err(e) => return escalation_failed(Phase::Render, e),
    };
    // renderフェーズの抽出は常にrender後URLを基準にする。結果を捨てる場合は本文ごと捨てる
    // ため、URLの出どころと本文の出どころが食い違わない。
    let rs = match build_stage(cli, &rendered.html, &rendered.final_url) {
        Ok(s) => s,
        Err(e) => return escalation_failed(Phase::Extract, e),
    };
    let status = choose_result(acq.stage.visible, rs.visible);
    let rendered_chars = Some(rs.visible);
    if status.is_rendered() {
        acq.stage = rs;
        acq.final_url = rendered.final_url;
    } else {
        eprintln!("webgrab: warn=auto-render-no-gain reason=shorter");
    }
    Ok(Escalation {
        status,
        rendered_chars,
    })
}

/// short-contentを通知するか。`content_len == 0`は`--max-chars 0`や末尾超過で、
/// 出力が空なのは本文が短いせいではないため通知しない。
fn is_short_content(route: Route, content_len: usize, total: usize) -> bool {
    route == Route::Extracted && content_len > 0 && total < SHORT_CONTENT_CHARS
}

/// 6〜8. 空本文チェック・文字量制御・通知・出力の組み立て。
fn assemble(cli: &Cli, acq: Acquired, status: RenderStatus, chars: CharCounts) -> Result<String> {
    let stage = acq.stage;
    // 6. 空本文チェック（--rawとtext/plainは免除、設計§4.3 4と設計10 §4.1）
    if stage.route == Route::Extracted && stage.body.trim().is_empty() {
        let (tok, prose) = hint_for(status);
        return Err(WebgrabError::new(
            ExitCode::Empty,
            format!("empty body extracted; retry with {prose}"),
        )
        .with_token("hint", tok));
    }

    // 7. 文字量制御・トークン
    let slice = budget::slice(&stage.body, cli.start_index, cli.max_chars);
    let tok = if cli.no_tokens {
        None
    } else {
        Some(tokens::count(&slice.content))
    };

    // 8. 短い本文の通知（提案はrender_status基準）
    let content_len = slice.content.chars().count();
    let (short_content, short_content_suggest) =
        if is_short_content(stage.route, content_len, slice.total) {
            let (hint, suggest) = hint_for(status);
            eprintln!(
                "webgrab: warn=short-content chars={} hint={hint}",
                slice.total
            );
            (Some(slice.total), suggest)
        } else {
            (None, "")
        };

    let meta = Meta {
        title: stage.title,
        url: acq.final_url,
        published_time: stage.published,
        tokens: tok,
        short_content,
        short_content_suggest,
        fence: cli.fence,
        render_status: status,
        static_chars: chars.static_chars,
        rendered_chars: chars.rendered_chars,
    };
    let extra = cli::extra_flags(cli, status);
    Ok(output::render(
        to_format(cli.format),
        &meta,
        &slice,
        cli.max_chars == 0,
        &extra,
    ))
}

/// `--wait-ms`はrender経路でしか効かない。
fn warn_ignored_flags(cli: &Cli) {
    if cli.wait_ms.is_some() && !cli.render && !cli.auto_render {
        eprintln!("webgrab: warn=flag-ignored flag=--wait-ms");
    }
}

/// CLIを実行し、最終出力文字列を返す。
pub async fn run(cli: &Cli) -> Result<String> {
    let start = Instant::now();
    let ua = cli
        .user_agent
        .clone()
        .unwrap_or_else(cli::default_user_agent);
    let timeout = Duration::from_secs(cli.timeout);
    warn_ignored_flags(cli);

    let (mut acq, mut status, mut chars) = if cli.render {
        let acq = render_phase(cli, ua, timeout).await?;
        let chars = CharCounts {
            static_chars: None,
            rendered_chars: Some(acq.stage.visible),
        };
        (acq, RenderStatus::Rendered, chars)
    } else {
        let acq = static_phase(cli, ua, timeout).await?;
        let chars = CharCounts {
            static_chars: Some(acq.stage.visible),
            rendered_chars: None,
        };
        (acq, RenderStatus::Static, chars)
    };

    // text/plainはChromeで描画しても同じテキストしか得られない（設計10 §3）。
    if cli.auto_render
        && !cli.render
        && acq.stage.route != Route::Plain
        && let Some(reason) = escalation_reason(acq.stage.visible)
    {
        let e = escalate(cli, &mut acq, timeout, start.elapsed(), reason).await?;
        status = e.status;
        chars.rendered_chars = e.rendered_chars;
    }

    assemble(cli, acq, status, chars)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_stage_passes_text_through() {
        let s = plain_stage("行1\nMutex<T>\n[x](javascript:a)\n");
        assert_eq!(s.body, "行1\nMutex<T>\n[x](unsafe-javascript:a)\n");
        assert_eq!(s.visible, s.body.chars().count());
        assert!(s.title.is_none());
        assert!(s.published.is_none());
        assert_eq!(s.route, Route::Plain);
        let empty = plain_stage("");
        assert_eq!(empty.body, "");
        assert_eq!(empty.visible, 0);
    }

    #[test]
    fn escalation_reason_thresholds() {
        assert_eq!(escalation_reason(0), Some("empty"));
        assert_eq!(escalation_reason(199), Some("short"));
        assert_eq!(escalation_reason(200), None);
    }

    #[test]
    fn remaining_budget_skips_below_thresholds() {
        let t = Duration::from_secs(30);
        assert!(matches!(
            remaining_budget(t, Duration::from_secs(26), 20 << 20, 0),
            Err(SkipReason::Timeout)
        ));
        assert!(matches!(
            remaining_budget(t, Duration::from_secs(1), 300 * 1024, 100 * 1024),
            Err(SkipReason::MaxBytes)
        ));
        let (rt, rb) = remaining_budget(t, Duration::from_secs(10), 20 << 20, 1 << 20).unwrap();
        assert_eq!(rt, Duration::from_secs(20));
        assert_eq!(rb, (20 << 20) - (1 << 20));
        assert!(
            matches!(
                remaining_budget(t, Duration::from_secs(40), 20 << 20, 0),
                Err(SkipReason::Timeout)
            ),
            "経過が予算超過なら0扱い"
        );
    }

    #[test]
    fn choose_result_prefers_longer() {
        assert_eq!(choose_result(150, 400), RenderStatus::Rendered);
        assert_eq!(choose_result(150, 150), RenderStatus::NoGain);
        assert_eq!(choose_result(150, 20), RenderStatus::NoGain);
    }

    #[test]
    fn fallback_reason_by_phase() {
        let e8 = WebgrabError::new(ExitCode::Netguard, "x");
        let e7 = WebgrabError::new(ExitCode::Render, "x");
        let e4 = WebgrabError::new(ExitCode::Http, "x");
        let e1 = WebgrabError::new(ExitCode::Internal, "x");
        assert_eq!(fallback_reason(Phase::Render, &e8), None);
        assert_eq!(fallback_reason(Phase::Render, &e7), Some("render"));
        assert_eq!(fallback_reason(Phase::Render, &e4), Some("max-bytes"));
        assert_eq!(fallback_reason(Phase::Render, &e1), None);
        assert_eq!(fallback_reason(Phase::Extract, &e4), Some("extract"));
        assert_eq!(fallback_reason(Phase::Extract, &e1), Some("extract"));
    }

    #[test]
    fn failure_detail_trims_trailing_space_when_no_detail() {
        let e = WebgrabError::new(ExitCode::Render, "chrome launch failed");
        assert_eq!(failure_detail(&e), "chrome launch failed");
        let e2 = WebgrabError::new(ExitCode::Render, "chrome launch failed")
            .with_detail("No such file or directory");
        assert_eq!(
            failure_detail(&e2),
            "chrome launch failed No such file or directory"
        );
    }

    #[test]
    fn hint_follows_render_status() {
        assert_eq!(
            hint_for(RenderStatus::Static),
            ("--render/--raw", "--render or --raw")
        );
        assert_eq!(
            hint_for(RenderStatus::Skipped("timeout")),
            ("--render/--raw", "--render or --raw")
        );
        assert_eq!(hint_for(RenderStatus::Rendered), ("--raw", "--raw"));
        assert_eq!(hint_for(RenderStatus::Failed("render")), ("--raw", "--raw"));
        assert_eq!(hint_for(RenderStatus::NoGain), ("--raw", "--raw"));
    }

    #[test]
    fn short_content_notice_boundaries() {
        assert!(is_short_content(Route::Extracted, 1, 199));
        assert!(
            !is_short_content(Route::Extracted, 1, 200),
            "200文字は短文でない"
        );
        assert!(
            !is_short_content(Route::Extracted, 0, 199),
            "出力が空なら通知しない"
        );
        assert!(!is_short_content(Route::Raw, 1, 199));
        assert!(!is_short_content(Route::Plain, 1, 199));
    }
}
