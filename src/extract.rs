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

/// HTMLのタグ開閉を線形走査し、要素ネストの最大深さが `limit` を超えるかを返す。
/// void要素・自己終了タグ・コメント/doctype/処理命令は深さに数えない。
pub(crate) fn exceeds_nesting_depth(html: &str, limit: usize) -> bool {
    let bytes = html.as_bytes();
    let mut depth = 0usize;
    let mut i = 0;
    while i < bytes.len() {
        let Some((step, end)) = scan_tag(html, i) else {
            i += 1;
            continue;
        };
        match step {
            DepthStep::Close => depth = depth.saturating_sub(1),
            DepthStep::Open => {
                depth += 1;
                if depth > limit {
                    return true;
                }
            }
            DepthStep::Neutral => {}
        }
        i = end;
    }
    false
}

/// タグが深さに与える影響。
enum DepthStep {
    Open,
    Close,
    Neutral,
}

/// `i`がタグの開始`<`なら、深さへの影響と次の走査位置（タグを閉じる`>`の直後）を返す。
/// コメント/doctype/処理命令と、`<`の後がタグ名でないリテラルの`<`は`None`。
fn scan_tag(html: &str, i: usize) -> Option<(DepthStep, usize)> {
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
    let k = bytes[j..]
        .iter()
        .position(|&b| b == b'>')
        .map_or(bytes.len(), |p| j + p);
    // `>`の直後から再開するので、各バイトは1度しか走査せず線形に収まる。
    // `>`が無いときkは末尾で、k + 1は走査を終える位置になる。
    let end = k + 1;
    if closing {
        return Some((DepthStep::Close, end));
    }
    let self_closing = bytes.get(k.wrapping_sub(1)) == Some(&b'/');
    let name = html[name_start..j].to_ascii_lowercase();
    let step = if self_closing || VOID.contains(&name.as_str()) {
        DepthStep::Neutral
    } else {
        DepthStep::Open
    };
    Some((step, end))
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
    fn nesting_depth_ignores_text_comments_and_literal_lt() {
        assert!(!exceeds_nesting_depth("abc", 0));
        assert!(!exceeds_nesting_depth("<!DOCTYPE html><?xml?>a < b", 0));
    }

    #[test]
    fn nesting_depth_balances_open_and_close_tags() {
        assert!(!exceeds_nesting_depth("<p></p><p></p>", 1));
        assert!(!exceeds_nesting_depth(
            r#"<p class="a"></p><p id="b"></p>"#,
            1
        ));
        assert!(exceeds_nesting_depth("x<p><p>", 1));
        assert!(exceeds_nesting_depth(r#"x<p class="a"><div id="b">"#, 1));
    }

    #[test]
    fn nesting_depth_skips_void_and_self_closing_tags() {
        assert!(!exceeds_nesting_depth(
            r#"<p/><br><BR><img src="a"><hr/>"#,
            0
        ));
        assert!(exceeds_nesting_depth("<my-el>", 0));
    }

    #[test]
    fn nesting_depth_skips_tags_inside_attribute_values() {
        assert!(!exceeds_nesting_depth(r#"<p title="<b x">"#, 1));
        assert!(!exceeds_nesting_depth(r#"<p a="x"/>"#, 0));
    }

    #[test]
    fn nesting_depth_counts_unterminated_last_tag() {
        assert!(exceeds_nesting_depth("x<p><div", 1));
        assert!(!exceeds_nesting_depth("x<p", 1));
    }

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
