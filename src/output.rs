//! 出力整形（設計§6、5形式）。

use crate::budget::{self, Slice};
use serde_json::json;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Markdown,
    Frontmatter,
    Json,
    Text,
    Html,
}

/// 出力に必要なメタデータ。
#[derive(Debug, Default)]
pub struct Meta {
    pub title: Option<String>,
    pub url: String,
    pub published_time: Option<String>,
    /// 概算トークン数（--no-tokensならNone）。
    pub tokens: Option<usize>,
}

/// スライス済み本文とメタから、指定形式の最終出力文字列を作る。
/// `body_slice`は既にstart/max適用済みの本文（markdown/text/htmlいずれか）。
/// `extra_flags`は継続コマンド再現用（-oと--start-indexは呼び出し側で除外済み）。
pub fn render(
    fmt: Format,
    meta: &Meta,
    slice: &Slice,
    max_chars_zero: bool,
    extra_flags: &[String],
) -> String {
    match fmt {
        Format::Markdown => render_markdown(meta, slice, false, max_chars_zero, extra_flags),
        Format::Frontmatter => render_markdown(meta, slice, true, max_chars_zero, extra_flags),
        Format::Json => render_json(meta, slice, max_chars_zero, extra_flags),
        Format::Text | Format::Html => render_plain(fmt, meta, slice, max_chars_zero, extra_flags),
    }
}

fn tokens_chars_line(meta: &Meta, slice: &Slice) -> String {
    let tok = match meta.tokens {
        Some(t) => t.to_string(),
        None => "-".to_string(),
    };
    format!(
        "Tokens: {} (chars: {} / total: {})",
        tok,
        slice.content.chars().count(),
        slice.total
    )
}

fn footer(meta: &Meta, slice: &Slice, extra_flags: &[String]) -> Option<String> {
    if slice.ended {
        Some(budget::end_footer(slice.total))
    } else if slice.truncated {
        Some(budget::truncated_footer(&meta.url, slice, extra_flags))
    } else {
        None
    }
}

fn render_markdown(
    meta: &Meta,
    slice: &Slice,
    frontmatter: bool,
    max_chars_zero: bool,
    extra_flags: &[String],
) -> String {
    let mut out = String::new();
    let published = meta.published_time.clone().unwrap_or_default();
    let title = meta.title.clone().unwrap_or_default();

    if frontmatter {
        out.push_str("---\n");
        out.push_str(&format!("title: {title}\n"));
        out.push_str(&format!("url: {}\n", meta.url));
        if meta.published_time.is_some() {
            out.push_str(&format!("published_time: {published}\n"));
        }
        out.push_str(&format!("chars: {}\n", slice.content.chars().count()));
        out.push_str(&format!("total_chars: {}\n", slice.total));
        out.push_str(&format!("truncated: {}\n", slice.truncated));
        out.push_str("---\n\n");
    } else {
        out.push_str(&format!("Title: {title}\n"));
        out.push_str(&format!("URL Source: {}\n", meta.url));
        if meta.published_time.is_some() {
            out.push_str(&format!("Published Time: {published}\n"));
        }
        out.push_str(&tokens_chars_line(meta, slice));
        out.push_str("\n\nMarkdown Content:\n");
    }

    out.push_str(&slice.content);

    if max_chars_zero {
        // メタのみ。本文は空。
    }
    if let Some(f) = footer(meta, slice, extra_flags) {
        out.push('\n');
        out.push_str(&f);
    }
    out
}

fn render_json(
    meta: &Meta,
    slice: &Slice,
    _max_chars_zero: bool,
    extra_flags: &[String],
) -> String {
    let continue_command = if slice.truncated {
        Some(budget::continue_command(&meta.url, slice.end, extra_flags))
    } else {
        None
    };
    let v = json!({
        "title": meta.title,
        "url": meta.url,
        "published_time": meta.published_time,
        "tokens": meta.tokens,
        "chars": slice.content.chars().count(),
        "total_chars": slice.total,
        "truncated": slice.truncated,
        "ended": slice.ended,
        "continue_command": continue_command,
        "markdown": slice.content,
    });
    v.to_string()
}

fn render_plain(
    fmt: Format,
    meta: &Meta,
    slice: &Slice,
    max_chars_zero: bool,
    extra_flags: &[String],
) -> String {
    let is_html = fmt == Format::Html;
    let mut out = String::new();

    if max_chars_zero {
        let line = format!("[webgrab:meta-only total {} chars]", slice.total);
        return wrap_marker(&line, is_html);
    }

    out.push_str(&slice.content);
    if let Some(f) = footer(meta, slice, extra_flags) {
        out.push('\n');
        out.push_str(&wrap_marker(&f, is_html));
    }
    out
}

fn wrap_marker(line: &str, is_html: bool) -> String {
    if is_html {
        format!("<!-- {line} -->")
    } else {
        line.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta() -> Meta {
        Meta {
            title: Some("T".into()),
            url: "https://x.test".into(),
            published_time: Some("2026-01-01T00:00:00Z".into()),
            tokens: Some(42),
        }
    }

    fn slc(content: &str, truncated: bool, ended: bool, total: usize) -> Slice {
        Slice {
            content: content.into(),
            start: 0,
            end: content.chars().count(),
            total,
            truncated,
            ended,
        }
    }

    #[test]
    fn markdown_has_jina_headers() {
        let s = slc("body", true, false, 100);
        let out = render(Format::Markdown, &meta(), &s, false, &[]);
        assert!(out.contains("Title: T"));
        assert!(out.contains("URL Source: https://x.test"));
        assert!(out.contains("Published Time: 2026-01-01T00:00:00Z"));
        assert!(out.contains("Markdown Content:"));
        assert!(out.contains("[webgrab:truncated"));
    }

    #[test]
    fn markdown_no_footer_when_complete() {
        let s = slc("body", false, false, 4);
        let out = render(Format::Markdown, &meta(), &s, false, &[]);
        assert!(!out.contains("webgrab:truncated"));
        assert!(!out.contains("webgrab:end"));
    }

    #[test]
    fn markdown_end_footer_when_ended() {
        let s = slc("", false, true, 500);
        let out = render(Format::Markdown, &meta(), &s, false, &[]);
        assert!(out.contains("[webgrab:end total 500 chars]"));
    }

    #[test]
    fn json_fields_present() {
        let s = slc("body", true, false, 100);
        let out = render(Format::Json, &meta(), &s, false, &["--render".into()]);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["truncated"], true);
        assert_eq!(v["total_chars"], 100);
        assert!(v["continue_command"].as_str().unwrap().contains("--render"));
        assert_eq!(v["markdown"], "body");
    }

    #[test]
    fn json_no_tokens_keeps_chars() {
        let mut m = meta();
        m.tokens = None;
        let s = slc("body", false, false, 4);
        let out = render(Format::Json, &m, &s, false, &[]);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert!(v["tokens"].is_null());
        assert_eq!(v["chars"], 4);
    }

    #[test]
    fn text_max_chars_zero_meta_only() {
        let s = slc("", false, false, 999);
        let out = render(Format::Text, &meta(), &s, true, &[]);
        assert_eq!(out, "[webgrab:meta-only total 999 chars]");
    }

    #[test]
    fn html_footer_is_comment() {
        let s = slc("<p>x</p>", true, false, 100);
        let out = render(Format::Html, &meta(), &s, false, &[]);
        assert!(out.contains("<!-- [webgrab:truncated"));
    }

    #[test]
    fn frontmatter_yaml_block() {
        let s = slc("body", false, false, 4);
        let out = render(Format::Frontmatter, &meta(), &s, false, &[]);
        assert!(out.starts_with("---\n"));
        assert!(out.contains("total_chars: 4"));
    }
}
