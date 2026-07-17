//! HTML→Markdown / テキスト変換（設計§4 convert、htmd）。

use crate::error::{ExitCode, Result, WebgrabError};

/// --raw変換の前処理として、`<script>` / `<style>` / `<noscript>` を要素ごと除去する。
/// これらの中身（JSコード・CSS）が本文に混入するのを防ぐ。抽出経路(dom_smoothie)は
/// 自前で除去するため、この関数は --raw のときだけ呼ぶ。
pub fn strip_non_content(html: &str) -> String {
    let mut out = html.to_string();
    for tag in ["script", "style", "noscript"] {
        out = remove_element(&out, tag);
    }
    out
}

/// `<tag ...>...</tag>` を要素ごと除去する（大小無視、複数対応）。
/// 開始タグ名の直後が区切り（空白/`>`/`/`）であることを確認し、`<scripts>`等の別タグは残す。
fn remove_element(html: &str, tag: &str) -> String {
    let lower = html.to_ascii_lowercase();
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    let mut out = String::with_capacity(html.len());
    let mut i = 0;
    while i < html.len() {
        if lower[i..].starts_with(&open) {
            let boundary = lower[i + open.len()..].chars().next();
            let is_tag = matches!(boundary, Some(' ' | '\t' | '\n' | '\r' | '>' | '/') | None);
            if is_tag {
                match lower[i..].find(&close) {
                    Some(rel) => {
                        i += rel + close.len();
                        continue;
                    }
                    // 閉じタグが無い場合は以降をすべて捨てる（壊れたHTMLの防御）
                    None => break,
                }
            }
        }
        let ch = html[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// HTMLをMarkdownへ変換する。危険なリンクスキームは無害化する。
pub fn to_markdown(html: &str) -> Result<String> {
    let md = htmd::convert(html).map_err(|e| {
        WebgrabError::new(ExitCode::Internal, "markdown convert failed").with_detail(e.to_string())
    })?;
    Ok(sanitize_link_schemes(&md))
}

/// Markdownリンク/画像ターゲット `](...)` のうち、クリックでスクリプトが走りうる
/// 実行系スキームだけを`unsafe-`接頭辞で無害化する（A03）。通常のURLや
/// `data:image/png`等の非実行データURLはそのまま残す。
fn sanitize_link_schemes(md: &str) -> String {
    const DANGER: &[&str] = &[
        "javascript:",
        "vbscript:",
        "data:text/html",
        "data:image/svg+xml",
    ];
    let mut out = String::with_capacity(md.len());
    let mut rest = md;
    while let Some(pos) = rest.find("](") {
        out.push_str(&rest[..pos + 2]);
        let after = &rest[pos + 2..];
        let lower = after.to_ascii_lowercase();
        if DANGER.iter().any(|d| lower.starts_with(d)) {
            out.push_str("unsafe-");
        }
        rest = after;
    }
    out.push_str(rest);
    out
}

/// HTMLからタグを除去したプレーンテキストを得る（--format text用）。
/// Markdownへ変換した後、行頭の見出し/引用/箇条書きの「記法」だけを落とす。
/// 本文そのものが `--` や `###` で始まる場合は削らない（データ欠損防止）。
pub fn to_text(html: &str) -> Result<String> {
    let md = to_markdown(html)?;
    let text = md
        .lines()
        .map(clean_text_line)
        .collect::<Vec<_>>()
        .join("\n");
    Ok(text)
}

/// 1行から行頭のMarkdown記法のみを除去し、htmdの行頭エスケープを1つ解除する。
fn clean_text_line(line: &str) -> String {
    let t = line.trim_start_matches(' ');
    let stripped = if let Some(r) = strip_atx_heading(t) {
        r
    } else if let Some(r) = t.strip_prefix("> ") {
        r
    } else if let Some(r) = t
        .strip_prefix("- ")
        .or_else(|| t.strip_prefix("* "))
        .or_else(|| t.strip_prefix("+ "))
    {
        r.trim_start_matches(' ')
    } else {
        t
    };
    unescape_leading_backslash(stripped)
}

/// `#`×1-6 + 空白 で始まる見出しなら、記号を除いた本文を返す。
fn strip_atx_heading(s: &str) -> Option<&str> {
    let hashes = s.chars().take_while(|&c| c == '#').count();
    if (1..=6).contains(&hashes) && s[hashes..].starts_with(' ') {
        Some(s[hashes..].trim_start_matches(' '))
    } else {
        None
    }
}

/// htmdが記法衝突回避のため付与した行頭の `\`（直後がASCII記号）を1つだけ外す。
fn unescape_leading_backslash(s: &str) -> String {
    let mut chars = s.chars();
    if chars.next() == Some('\\')
        && chars
            .clone()
            .next()
            .is_some_and(|c| c.is_ascii_punctuation())
    {
        return s[1..].to_string();
    }
    s.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_headings_links_code() {
        let html = "<h1>Title</h1><p>See <a href=\"https://x.test\">link</a>.</p><pre><code>fn main(){}</code></pre>";
        let md = to_markdown(html).unwrap();
        assert!(md.contains("# Title"));
        assert!(md.contains("[link](https://x.test)"));
        assert!(md.contains("fn main(){}"));
    }

    #[test]
    fn dangerous_link_schemes_neutralized() {
        // クリックでスクリプト実行しうるスキームだけ無害化（A03）。
        let md = to_markdown(r#"<a href="javascript:alert(1)">x</a>"#).unwrap();
        assert!(md.contains("](unsafe-javascript:"), "js未無害化: {md}");
        assert!(!md.contains("](javascript:"));

        let md2 = to_markdown(r#"<a href="VBScript:msgbox(1)">y</a>"#).unwrap();
        assert!(md2.to_ascii_lowercase().contains("](unsafe-vbscript:"));

        let md3 = to_markdown(r#"<a href="data:text/html,<script>1</script>">z</a>"#).unwrap();
        assert!(md3.contains("](unsafe-data:text/html"));
    }

    #[test]
    fn safe_links_and_data_images_untouched() {
        let md = to_markdown(r#"<a href="https://ok.test/p">o</a>"#).unwrap();
        assert!(md.contains("](https://ok.test/p)"));
        assert!(!md.contains("unsafe-"));
        // data:image/png は実行系でないため触らない
        let md2 = to_markdown(r#"<img src="data:image/png;base64,iVBORw0KGgo=">"#).unwrap();
        assert!(!md2.contains("unsafe-"), "data:imageを誤って無害化: {md2}");
    }

    #[test]
    fn strip_non_content_removes_script_style_noscript() {
        // --raw変換前に script/style/noscript を要素ごと除去する（本文ノイズ対策）。
        let html = "<style>@font-face{src:url(x)}</style><p>本文テキスト</p>\
            <script>var a=1; function f(){}</script><noscript>NOSCRIPT</noscript>";
        let out = strip_non_content(html);
        assert!(out.contains("本文テキスト"));
        assert!(!out.contains("font-face"), "styleが残存: {out}");
        assert!(!out.contains("function"), "scriptが残存: {out}");
        assert!(!out.contains("NOSCRIPT"), "noscriptが残存: {out}");
        assert!(!out.to_ascii_lowercase().contains("<script"));
    }

    #[test]
    fn strip_non_content_case_insensitive_and_attrs() {
        let html = "<SCRIPT type=\"text/js\">x</SCRIPT><STYLE>y</STYLE><p>keep</p>";
        let out = strip_non_content(html);
        assert!(out.contains("keep"));
        assert!(!out.to_ascii_lowercase().contains("script"));
        assert!(!out.to_ascii_lowercase().contains("style"));
    }

    #[test]
    fn strip_non_content_keeps_similar_tags() {
        // <scripts> や <article> のような別タグは削らない。
        let html = "<article>本文</article>";
        assert_eq!(strip_non_content(html), "<article>本文</article>");
    }

    #[test]
    fn to_markdown_raw_page_has_no_script_noise() {
        let html = "<html><head><style>@font-face{a:b}</style></head>\
            <body><script>function f(){}</script><p>本文だけ残す</p></body></html>";
        let md = to_markdown(&strip_non_content(html)).unwrap();
        assert!(md.contains("本文だけ残す"));
        assert!(!md.contains("function"));
        assert!(!md.contains("font-face"));
    }

    #[test]
    fn converts_table() {
        let html = "<table><tr><th>A</th><th>B</th></tr><tr><td>1</td><td>2</td></tr></table>";
        let md = to_markdown(html).unwrap();
        assert!(md.contains("| A | B |"));
        assert!(md.contains("| 1 | 2 |"));
    }

    #[test]
    fn text_strips_heading_markers() {
        let html = "<h1>Title</h1><p>body</p>";
        let t = to_text(html).unwrap();
        assert!(t.contains("Title"));
        assert!(!t.contains("# Title"));
    }

    #[test]
    fn text_preserves_literal_leading_dashes() {
        // 本文が "--" で始まる場合に先頭が削られないこと（データ欠損の回帰ガード）。
        let html = "<p>-- END OF REPORT --</p>";
        let t = to_text(html).unwrap();
        assert!(t.contains("-- END OF REPORT --"), "先頭が欠損: {t:?}");
    }

    #[test]
    fn text_unescapes_leading_backslash() {
        // htmdが付与した行頭エスケープ "\#" 等の生バックスラッシュを残さない。
        let html = "<p>### literal text</p>";
        let t = to_text(html).unwrap();
        assert!(!t.contains('\\'), "バックスラッシュ残存: {t:?}");
        assert!(t.contains("### literal text"), "本文欠損: {t:?}");
    }

    #[test]
    fn text_strips_list_marker() {
        let html = "<ul><li>item one</li></ul>";
        let t = to_text(html).unwrap();
        assert!(t.contains("item one"));
        assert!(!t.trim_start().starts_with('*'));
    }
}
