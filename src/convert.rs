//! HTML→Markdown / テキスト変換（設計§4 convert、htmd）。

use crate::error::{ExitCode, Result, WebgrabError};

/// HTMLをMarkdownへ変換する。
pub fn to_markdown(html: &str) -> Result<String> {
    htmd::convert(html).map_err(|e| {
        WebgrabError::new(ExitCode::Internal, "markdown convert failed").with_detail(e.to_string())
    })
}

/// HTMLからタグを除去したプレーンテキストを得る（--format text用）。
/// Markdownへ変換した後、記法記号を最小限そのまま残す簡易実装。
pub fn to_text(html: &str) -> Result<String> {
    // htmdのMarkdownを土台に、行頭の見出し記号・強調記号を軽く落とす。
    let md = to_markdown(html)?;
    let text = md
        .lines()
        .map(|l| l.trim_start_matches(['#', '>', '-', '*', ' ']))
        .collect::<Vec<_>>()
        .join("\n");
    Ok(text)
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
}
