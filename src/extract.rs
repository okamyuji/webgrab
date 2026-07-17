//! 本文抽出（設計§4 extract、dom_smoothie）。

use crate::error::{ExitCode, Result, WebgrabError};

/// 要素ネスト深さの上限。dom_smoothieのスコアリングはネスト深さに対し
/// ほぼ3乗で増大し、数千段で実質ハングする。実ページはまず数百段を超えないため、
/// この上限を超える入力は抽出前に打ち切りDoSを防ぐ。
const MAX_NESTING_DEPTH: usize = 1000;

/// 抽出結果。
#[derive(Debug, Default)]
pub struct Extracted {
    pub title: Option<String>,
    pub published_time: Option<String>,
    /// 本文HTML（--rawでない場合はdom_smoothieの抽出結果）。
    pub content_html: String,
}

/// HTMLのタグ開閉を線形走査し、要素ネストの最大深さが `limit` を超えるかを返す。
/// void要素・自己終了タグ・コメント/doctype/処理命令は深さに数えない。
fn exceeds_nesting_depth(html: &str, limit: usize) -> bool {
    const VOID: &[&str] = &[
        "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param",
        "source", "track", "wbr",
    ];
    let bytes = html.as_bytes();
    let mut depth = 0usize;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'<' {
            i += 1;
            continue;
        }
        // コメント/doctype/処理命令はスキップ
        if bytes.get(i + 1) == Some(&b'!') || bytes.get(i + 1) == Some(&b'?') {
            i += 1;
            continue;
        }
        let closing = bytes.get(i + 1) == Some(&b'/');
        let name_start = if closing { i + 2 } else { i + 1 };
        // タグ名を取得
        let mut j = name_start;
        while j < bytes.len() && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'-') {
            j += 1;
        }
        if j == name_start {
            i += 1; // '<' の後がタグ名でない（`<` リテラル等）
            continue;
        }
        let name = html[name_start..j].to_ascii_lowercase();
        // タグ終端 '>' を探し、自己終了 '/>' か判定
        let mut k = j;
        while k < bytes.len() && bytes[k] != b'>' {
            k += 1;
        }
        let self_closing = k > 0 && bytes.get(k.wrapping_sub(1)) == Some(&b'/');
        if closing {
            depth = depth.saturating_sub(1);
        } else if !self_closing && !VOID.contains(&name.as_str()) {
            depth += 1;
            if depth > limit {
                return true;
            }
        }
        i = if k < bytes.len() { k + 1 } else { k };
    }
    false
}

/// HTMLから本文・title・公開日時を抽出する。
/// `base_url`はdom_smoothieの相対リンク解決に使う。
pub fn extract(html: &str, base_url: &str) -> Result<Extracted> {
    if exceeds_nesting_depth(html, MAX_NESTING_DEPTH) {
        return Err(
            WebgrabError::new(ExitCode::Http, "html too deeply nested for extraction").with_detail(
                format!(
                    "nesting depth exceeds {MAX_NESTING_DEPTH}; retry with --raw to skip extraction"
                ),
            ),
        );
    }
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
    fn deeply_nested_html_is_rejected_fast() {
        // 深いネストはdom_smoothieのスコアリングが3乗的に膨張しハングする。
        // 抽出前に線形の深さガードで打ち切り、DoSを防ぐ（回帰ガード）。
        let deep = format!("{}x{}", "<div>".repeat(5000), "</div>".repeat(5000));
        let e = extract(&deep, "https://example.com");
        assert!(e.is_err(), "深いネストが拒否されずに通過した");
    }

    #[test]
    fn normal_depth_still_extracts() {
        let html = format!(
            "<html><body><article>{}<p>{}</p></article></body></html>",
            "<div>".repeat(50).to_string() + &"</div>".repeat(50),
            "これは十分な長さの本文です。抽出のために意味のある文章を複数入れます。さらに続けます。"
        );
        assert!(extract(&html, "https://example.com").is_ok());
    }

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
