//! パイプライン結線（設計§4）。fetch|render → decode → extract → convert → budget → output。

use crate::cli::{Cli, FormatArg};
use crate::error::{ExitCode, Result, WebgrabError};
use crate::fetch::{self, FetchOptions};
use crate::output::{self, Format, Meta};
use crate::render::{self, RenderOptions};
use crate::{budget, cli, convert, decode, extract, tokens};
use std::time::Duration;

fn to_format(f: FormatArg) -> Format {
    match f {
        FormatArg::Markdown => Format::Markdown,
        FormatArg::Frontmatter => Format::Frontmatter,
        FormatArg::Json => Format::Json,
        FormatArg::Text => Format::Text,
        FormatArg::Html => Format::Html,
    }
}

/// CLIを実行し、最終出力文字列を返す。
pub async fn run(cli: &Cli) -> Result<String> {
    let ua = cli
        .user_agent
        .clone()
        .unwrap_or_else(cli::default_user_agent);
    let timeout = Duration::from_secs(cli.timeout);

    // 1. 取得（HTML文字列とfinal_url、title候補）
    let (html, final_url, header_title): (String, String, Option<String>) = if cli.render {
        let ropts = RenderOptions {
            timeout,
            wait_ms: cli.wait_ms,
            allow_private: cli.allow_private,
            chrome_path: cli.chrome_path.clone(),
        };
        let dom = render::render(&cli.url, &ropts).await?;
        (dom, cli.url.clone(), None)
    } else {
        let fopts = FetchOptions {
            user_agent: ua,
            timeout,
            max_bytes: cli.max_bytes,
            allow_private: cli.allow_private,
            check_robots: !cli.no_robots,
        };
        let fetched = fetch::fetch(&cli.url, &fopts).await?;
        let (text, _enc) = decode::decode(&fetched.body, fetched.content_type.as_deref());
        (text, fetched.final_url, None)
    };
    let _ = header_title;

    // 2. 抽出 or raw
    let (title, published, body_html) = if cli.raw {
        (None, None, html.clone())
    } else {
        let ex = extract::extract(&html, &final_url)?;
        (ex.title, ex.published_time, ex.content_html)
    };

    // 3. 変換（形式に応じて）
    let fmt = to_format(cli.format);
    let body: String = match fmt {
        Format::Html => body_html.clone(),
        Format::Text => convert::to_text(&body_html)?,
        _ => convert::to_markdown(&body_html)?,
    };

    // 4. 空本文チェック（--rawでない場合、設計§7）
    if !cli.raw && body.trim().is_empty() {
        return Err(WebgrabError::new(ExitCode::Empty, "empty body extracted")
            .with_detail("hint: retry with --raw or --render"));
    }

    // 5. 文字量制御
    let slice = budget::slice(&body, cli.start_index, cli.max_chars);
    let max_chars_zero = cli.max_chars == 0;

    // 6. トークン計測
    let tok = if cli.no_tokens {
        None
    } else {
        Some(tokens::count(&slice.content))
    };

    // 7. 短い本文の警告（設計§5、1〜199文字かつ静的取得時）
    let content_len = slice.content.chars().count();
    if !cli.raw && !cli.render && content_len > 0 && slice.total < 200 {
        eprintln!(
            "webgrab: warn=short-content chars={} hint=--render",
            slice.total
        );
    }

    let meta = Meta {
        title,
        url: final_url,
        published_time: published,
        tokens: tok,
    };
    let extra = cli::extra_flags(cli);
    Ok(output::render(fmt, &meta, &slice, max_chars_zero, &extra))
}
