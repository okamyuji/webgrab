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
pub enum Phase { Render, Extract }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason { Timeout, MaxBytes }

impl SkipReason {
    pub fn token(self) -> &'static str {
        match self { SkipReason::Timeout => "timeout", SkipReason::MaxBytes => "max-bytes" }
    }
}

pub fn escalation_reason(visible_chars: usize) -> Option<&'static str> {
    if visible_chars == 0 { Some("empty") } else if visible_chars < SHORT_CONTENT_CHARS { Some("short") } else { None }
}

pub fn remaining_budget(timeout: Duration, elapsed: Duration, max_bytes: u64, consumed: u64) -> std::result::Result<(Duration, u64), SkipReason> {
    let rt = timeout.saturating_sub(elapsed);
    if rt < SKIP_TIMEOUT_MIN { return Err(SkipReason::Timeout); }
    let rb = max_bytes.saturating_sub(consumed);
    if rb < SKIP_BYTES_MIN { return Err(SkipReason::MaxBytes); }
    Ok((rt, rb))
}

pub fn choose_result(static_chars: usize, rendered_chars: usize) -> RenderStatus {
    if rendered_chars > static_chars { RenderStatus::Rendered } else { RenderStatus::NoGain }
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
        RenderStatus::Rendered | RenderStatus::Failed(_) | RenderStatus::NoGain => ("--raw", "--raw"),
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

/// 抽出・変換済みの中間結果。
struct Stage {
    title: Option<String>,
    published: Option<String>,
    body: String,
    visible: usize,
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
    Ok(Stage { title, published, body, visible })
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

/// CLIを実行し、最終出力文字列を返す。
pub async fn run(cli: &Cli) -> Result<String> {
    let start = Instant::now();
    let ua = cli.user_agent.clone().unwrap_or_else(cli::default_user_agent);
    let timeout = Duration::from_secs(cli.timeout);
    if cli.wait_ms.is_some() && !cli.render && !cli.auto_render {
        eprintln!("webgrab: warn=flag-ignored flag=--wait-ms");
    }
    if cli.no_sandbox && (cli.render || cli.auto_render) {
        eprintln!("webgrab: warn=no-sandbox");
    }

    // 1. 静的フェーズ（または --render 明示）
    let mut status = RenderStatus::Static;
    let mut static_chars: Option<usize> = None;
    let mut rendered_chars: Option<usize> = None;
    let (html, final_url, consumed) = if cli.render {
        if !cli.no_robots {
            let fopts = FetchOptions { user_agent: ua, timeout, max_bytes: cli.max_bytes, allow_private: cli.allow_private, check_robots: true };
            if !fetch::robots_precheck(&cli.url, &fopts).await? {
                return Err(WebgrabError::new(ExitCode::Robots, "blocked by robots.txt").with_detail(format!("url={}", cli.url)));
            }
        }
        let dom = render::render(&cli.url, &render_options(cli, timeout, cli.max_bytes)).await?;
        status = RenderStatus::Rendered;
        (dom, cli.url.clone(), 0u64)
    } else {
        let fopts = FetchOptions { user_agent: ua, timeout, max_bytes: cli.max_bytes, allow_private: cli.allow_private, check_robots: !cli.no_robots };
        let fetched = fetch::fetch(&cli.url, &fopts).await?;
        let (text, enc, had_errors) = decode::decode(&fetched.body, fetched.content_type.as_deref());
        if had_errors {
            eprintln!("webgrab: warn=decode-replacement enc={enc}");
        }
        (text, fetched.final_url, fetched.consumed_bytes)
    };

    let mut stage = build_stage(cli, &html, &final_url)?;
    if cli.render {
        rendered_chars = Some(stage.visible);
    } else {
        static_chars = Some(stage.visible);
    }

    // 2〜5. エスカレーション（--auto-render、--render明示時は無効）
    if cli.auto_render && !cli.render
        && let Some(reason) = escalation_reason(stage.visible)
    {
        match remaining_budget(timeout, start.elapsed(), cli.max_bytes, consumed) {
            Err(skip) => {
                eprintln!("webgrab: warn=auto-render-skipped reason={}", skip.token());
                status = RenderStatus::Skipped(skip.token());
            }
            Ok((rt, rb)) => {
                eprintln!("webgrab: info=auto-render reason={reason} chars={}", stage.visible);
                match render::render(&final_url, &render_options(cli, rt, rb)).await {
                    Ok(dom) => match build_stage(cli, &dom, &final_url) {
                        Ok(rs) => {
                            rendered_chars = Some(rs.visible);
                            status = choose_result(stage.visible, rs.visible);
                            if status.is_rendered() {
                                stage = rs;
                            } else {
                                eprintln!("webgrab: warn=auto-render-no-gain reason=shorter");
                            }
                        }
                        Err(e) => match fallback_reason(Phase::Extract, &e) {
                            Some(r) => {
                                eprintln!("webgrab: warn=auto-render-failed reason={r}");
                                eprintln!("{}", crate::error::sanitize_detail(&e.message));
                                status = RenderStatus::Failed(r);
                            }
                            None => return Err(e),
                        },
                    },
                    Err(e) => match fallback_reason(Phase::Render, &e) {
                        Some(r) => {
                            eprintln!("webgrab: warn=auto-render-failed reason={r}");
                            eprintln!("{}", crate::error::sanitize_detail(&format!("{} {}", e.message, e.detail.as_deref().unwrap_or(""))));
                            status = RenderStatus::Failed(r);
                        }
                        None => return Err(e),
                    },
                }
            }
        }
    }

    // 6. 空本文チェック（--rawは免除、設計§4.3 4）
    if !cli.raw && stage.body.trim().is_empty() {
        let (tok, prose) = hint_for(status);
        return Err(WebgrabError::new(ExitCode::Empty, format!("empty body extracted; retry with {prose}")).with_token("hint", tok));
    }

    // 7. 文字量制御・トークン
    let slice = budget::slice(&stage.body, cli.start_index, cli.max_chars);
    let max_chars_zero = cli.max_chars == 0;
    let tok = if cli.no_tokens { None } else { Some(tokens::count(&slice.content)) };

    // 8. 短い本文の通知（提案はrender_status基準）
    let content_len = slice.content.chars().count();
    let (short_content, short_content_suggest) = if !cli.raw && content_len > 0 && slice.total < SHORT_CONTENT_CHARS {
        let (hint, suggest) = hint_for(status);
        eprintln!("webgrab: warn=short-content chars={} hint={hint}", slice.total);
        (Some(slice.total), suggest)
    } else {
        (None, "")
    };

    let meta = Meta {
        title: stage.title,
        url: final_url,
        published_time: stage.published,
        tokens: tok,
        short_content,
        short_content_suggest,
        fence: cli.fence,
        render_status: status,
        static_chars,
        rendered_chars,
    };
    let extra = cli::extra_flags(cli, status);
    Ok(output::render(to_format(cli.format), &meta, &slice, max_chars_zero, &extra))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escalation_reason_thresholds() {
        assert_eq!(escalation_reason(0), Some("empty"));
        assert_eq!(escalation_reason(199), Some("short"));
        assert_eq!(escalation_reason(200), None);
    }

    #[test]
    fn remaining_budget_skips_below_thresholds() {
        let t = Duration::from_secs(30);
        assert!(matches!(remaining_budget(t, Duration::from_secs(26), 20 << 20, 0), Err(SkipReason::Timeout)));
        assert!(matches!(remaining_budget(t, Duration::from_secs(1), 300 * 1024, 100 * 1024), Err(SkipReason::MaxBytes)));
        let (rt, rb) = remaining_budget(t, Duration::from_secs(10), 20 << 20, 1 << 20).unwrap();
        assert_eq!(rt, Duration::from_secs(20));
        assert_eq!(rb, (20 << 20) - (1 << 20));
        assert!(matches!(remaining_budget(t, Duration::from_secs(40), 20 << 20, 0), Err(SkipReason::Timeout)), "経過が予算超過なら0扱い");
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
    fn hint_follows_render_status() {
        assert_eq!(hint_for(RenderStatus::Static), ("--render/--raw", "--render or --raw"));
        assert_eq!(hint_for(RenderStatus::Skipped("timeout")), ("--render/--raw", "--render or --raw"));
        assert_eq!(hint_for(RenderStatus::Rendered), ("--raw", "--raw"));
        assert_eq!(hint_for(RenderStatus::Failed("render")), ("--raw", "--raw"));
        assert_eq!(hint_for(RenderStatus::NoGain), ("--raw", "--raw"));
    }
}
