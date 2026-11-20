//! 本文抽出（設計§4 extract、dom_smoothie）。

use crate::error::{ExitCode, Result, WebgrabError};

/// 要素ネスト深さの上限。dom_smoothieのスコアリングはネスト深さに対し
/// ほぼ3乗で増大し、数千段で実質ハングする。実ページはまず数百段を超えないため、
/// この上限を超える入力は抽出前に打ち切りDoSを防ぐ。
pub(crate) const MAX_NESTING_DEPTH: usize = 1000;

/// 抽出結果。
#[derive(Debug, Default)]
pub struct Extracted {
    pub title: Option<String>,
    pub published_time: Option<String>,
    /// 本文HTML（--rawでない場合はdom_smoothieの抽出結果）。
    pub content_html: String,
}

/// HTML5パーサーが作る要素の木で、入れ子の最大深さが`limit`を超えるかを返す。
/// 字句で数えると`<div/>`や対応の無い閉じタグのように開いたまま残る形を取り逃がすため、
/// 変換や抽出と同じ解釈の木を数える。木は再帰せずスタックでたどる。
pub(crate) fn exceeds_nesting_depth(html: &str, limit: usize) -> bool {
    let doc = dom_query::Document::from(html);
    let mut stack: Vec<_> = doc
        .root()
        .element_children()
        .into_iter()
        .map(|n| (n, 1))
        .collect();
    while let Some((node, depth)) = stack.pop() {
        if depth > limit {
            return true;
        }
        let mut child = node.first_element_child();
        while let Some(c) = child {
            child = c.next_element_sibling();
            stack.push((c, depth + 1));
        }
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
    let article = match readability.parse() {
        Ok(a) => a,
        Err(dom_smoothie::ReadabilityError::GrabFailed) => {
            eprintln!("webgrab: warn=extract-grab-failed");
            return Ok(Extracted::default());
        }
        Err(e) => {
            return Err(
                WebgrabError::new(ExitCode::Internal, "readability parse failed")
                    .with_detail(e.to_string()),
            );
        }
    };

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

    // 深さはパーサーが作る木で数える。文書には常にhtmlとbodyがあり、それだけで深さ2になる。
    #[test]
    fn nesting_depth_counts_html_and_body() {
        assert!(!exceeds_nesting_depth("abc", 2));
        assert!(exceeds_nesting_depth("abc", 1));
        assert!(!exceeds_nesting_depth("<!DOCTYPE html><?xml?>a < b <1", 2));
    }

    #[test]
    fn nesting_depth_follows_open_and_close_tags() {
        assert!(!exceeds_nesting_depth(
            r#"<p class="a"></p><p id="b"></p>"#,
            3
        ));
        assert!(exceeds_nesting_depth("<div><div></div></div>", 3));
        assert!(!exceeds_nesting_depth("<div><div></div></div>", 4));
    }

    #[test]
    fn nesting_depth_does_not_count_void_elements() {
        assert!(!exceeds_nesting_depth(
            &r#"<br><BR><img src="a"><hr/>"#.repeat(50),
            3
        ));
    }

    #[test]
    fn nesting_depth_does_not_count_implicitly_closed_list_items() {
        let list = format!("<ul>{}</ul>", "<li>item".repeat(2000));
        assert!(!exceeds_nesting_depth(&list, 4));
    }

    #[test]
    fn nesting_depth_counts_forms_the_parser_keeps_open() {
        // HTML5では非void要素の`/>`、対応の無い閉じタグ、引用符内の`/>`は要素を閉じない。
        for unit in ["<div/>", "<span/>", "<div></i>", r#"<div a="/>">"#] {
            assert!(exceeds_nesting_depth(&unit.repeat(50), 40), "{unit}");
        }
    }

    #[test]
    fn deeply_nested_html_is_rejected_fast() {
        // 深いネストはdom_smoothieのスコアリングが3乗的に膨張しハングする。
        // 抽出前に深さの判定で打ち切り、DoSを防ぐ（回帰ガード）。
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

    #[test]
    fn empty_shell_maps_to_empty_body_not_error() {
        // JSで本文を後から入れる空シェル。dom_smoothieはGrabFailedを返すが、Ok(空)にする（設計§4.1）。
        let e = extract(
            "<html><body><div id=\"app\"></div></body></html>",
            "https://x.test",
        )
        .unwrap();
        assert!(e.content_html.trim().is_empty());
    }

    #[test]
    fn placeholder_text_is_extracted_as_short_body() {
        // F2の再現markup。8文字が抽出される（E7の前提を固定）。
        let e = extract(
            "<html><body><div id=\"app\">読み込み中...</div></body></html>",
            "https://x.test",
        )
        .unwrap();
        let text = crate::convert::to_text(&e.content_html).unwrap();
        assert_eq!(text.trim(), "読み込み中...");
    }
}
