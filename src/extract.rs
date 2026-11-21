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

/// 入れ子の最大深さが`limit`を超えるかを返す。
/// html5everは深い木の構築に二乗時間がかかるため、まず線形の字句判定で拒否する。
/// 字句判定は`<div/>`や対応の無い閉じタグのように開いたまま残る形を浅く数えるので、
/// 字句判定を通った入力は、変換や抽出と同じ解釈の木でも数える。
pub(crate) fn exceeds_nesting_depth(html: &str, limit: usize) -> bool {
    lexical_depth_exceeds(html, limit) || tree_depth_exceeds(html, limit)
}

/// タグの開閉を線形に走査して数えた深さが`limit`を超えるか。void要素は数えない。
/// 閉じタグを省いた`<li>`や`<p>`は多めに数える。
fn lexical_depth_exceeds(html: &str, limit: usize) -> bool {
    let bytes = html.as_bytes();
    let mut depth = 0usize;
    let mut i = 0;
    while i < bytes.len() {
        match scan_tag(html, i) {
            Some((TagKind::Close, next)) => {
                depth = depth.saturating_sub(1);
                i = next;
            }
            Some((TagKind::Open, next)) => {
                depth += 1;
                if depth > limit {
                    return true;
                }
                i = next;
            }
            Some((TagKind::Void, next)) => i = next,
            None => i += 1,
        }
    }
    false
}

enum TagKind {
    Open,
    Close,
    Void,
}

/// `i`がタグの開始`<`なら、種類と、タグを閉じる`>`の位置（無ければ末尾）を返す。
/// `>`の位置から走査を続けるので、各バイトは1度しか見ない。
/// コメント/doctype/処理命令と、`<`の後がタグ名でないリテラルの`<`は`None`。
fn scan_tag(html: &str, i: usize) -> Option<(TagKind, usize)> {
    const VOID: &[&str] = &[
        "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param",
        "source", "track", "wbr",
    ];
    let bytes = html.as_bytes();
    if bytes[i] != b'<' || matches!(bytes.get(i + 1), Some(b'!' | b'?')) {
        return None;
    }
    let closing = bytes.get(i + 1) == Some(&b'/');
    let name_start = if closing { i + 2 } else { i + 1 };
    let name_len = bytes[name_start.min(bytes.len())..]
        .iter()
        .take_while(|b| b.is_ascii_alphanumeric() || **b == b'-')
        .count();
    if name_len == 0 {
        return None;
    }
    let j = name_start + name_len;
    let gt = bytes[j..]
        .iter()
        .position(|&b| b == b'>')
        .map_or(bytes.len(), |p| j + p);
    let kind = if closing {
        TagKind::Close
    } else if VOID.contains(&html[name_start..j].to_ascii_lowercase().as_str()) {
        TagKind::Void
    } else {
        TagKind::Open
    };
    Some((kind, gt))
}

/// HTML5パーサーが作る要素の木で深さを数える。木は再帰せずスタックでたどる。
fn tree_depth_exceeds(html: &str, limit: usize) -> bool {
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

    #[test]
    fn plain_deep_nesting_is_rejected_without_building_the_tree() {
        // 木を作るhtml5everは深い入力で二乗時間になるため、字句の判定で先に拒否する。
        let deep = "<div>".repeat(200_000);
        let start = std::time::Instant::now();
        assert!(exceeds_nesting_depth(&deep, MAX_NESTING_DEPTH));
        assert!(start.elapsed() < std::time::Duration::from_secs(5));
    }

    #[test]
    fn lexical_depth_counts_open_close_and_void_tags() {
        assert!(!lexical_depth_exceeds("abc <!-- x --> a < b", 0));
        assert!(!lexical_depth_exceeds("<p></p><p></p>", 1));
        assert!(!lexical_depth_exceeds(
            r#"<p class="a"></p><p id="b"></p>"#,
            1
        ));
        assert!(lexical_depth_exceeds("x<p><p>", 1));
        assert!(lexical_depth_exceeds(r#"x<p class="a"><div id="b">"#, 1));
        assert!(!lexical_depth_exceeds(r#"<br><BR><img src="a"><hr/>"#, 0));
        assert!(lexical_depth_exceeds("<my-el>", 0));
        assert!(lexical_depth_exceeds("x<p><div", 1));
        assert!(!lexical_depth_exceeds("x<p", 1));
        // タグは最初の`>`まで読み飛ばすので、属性値の中の`<b`は数えない。
        assert!(!lexical_depth_exceeds(r#"<p title="<b x">"#, 1));
    }

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
    fn tree_depth_does_not_count_implicitly_closed_list_items() {
        // 字句判定はこの形を多めに数えて先に拒否するが、木では深さ4に収まる。
        let list = format!("<ul>{}</ul>", "<li>item".repeat(2000));
        assert!(!tree_depth_exceeds(&list, 4));
        assert!(lexical_depth_exceeds(&list, 4));
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
