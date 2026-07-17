//! 本文抽出（設計§4 extract、dom_smoothie）。

use crate::error::{ExitCode, Result, WebgrabError};

/// 抽出結果。
#[derive(Debug, Default)]
pub struct Extracted {
    pub title: Option<String>,
    pub published_time: Option<String>,
    /// 本文HTML（--rawでない場合はdom_smoothieの抽出結果）。
    pub content_html: String,
}

/// HTMLから本文・title・公開日時を抽出する。
/// `base_url`はdom_smoothieの相対リンク解決に使う。
pub fn extract(html: &str, base_url: &str) -> Result<Extracted> {
    let cfg = dom_smoothie::Config::default();
    let mut readability =
        dom_smoothie::Readability::new(html, Some(base_url), Some(cfg)).map_err(|e| {
            WebgrabError::new(ExitCode::Internal, "readability init failed")
                .with_detail(e.to_string())
        })?;
    let article = readability.parse().map_err(|e| {
        WebgrabError::new(ExitCode::Internal, "readability parse failed").with_detail(e.to_string())
    })?;

    let title = {
        let t = article.title.to_string();
        if t.trim().is_empty() { None } else { Some(t) }
    };
    Ok(Extracted {
        title,
        published_time: article.published_time.map(|s| s.to_string()),
        content_html: article.content.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_title_and_published_time() {
        let doc = r#"<html><head><title>記事タイトル</title>
          <meta property="article:published_time" content="2026-01-02T03:04:05Z">
          </head><body><article><h1>記事タイトル</h1>
          <p>これは十分な長さの本文です。抽出アルゴリズムが本文と判定するために、意味のある文章を複数入れておきます。さらに文章を続けます。</p>
          </article></body></html>"#;
        let e = extract(doc, "https://example.com").unwrap();
        assert_eq!(e.published_time.as_deref(), Some("2026-01-02T03:04:05Z"));
        assert!(e.content_html.contains("本文"));
    }
}
