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
/// `</tag` の直後に空白（HTMLとして正当）を挟んで `>` が来る閉じタグを探す。
/// 見つかった場合、`haystack` 先頭からの終端直後のバイトオフセットを返す。
fn find_close_tag_end(haystack: &str, close_prefix: &str) -> Option<usize> {
    let bytes = haystack.as_bytes();
    let mut search_start = 0;
    while let Some(rel) = haystack[search_start..].find(close_prefix) {
        let match_start = search_start + rel;
        let mut j = match_start + close_prefix.len();
        while j < bytes.len() && matches!(bytes[j], b' ' | b'\t' | b'\n' | b'\r') {
            j += 1;
        }
        if j < bytes.len() && bytes[j] == b'>' {
            return Some(j + 1);
        }
        search_start = match_start + 1;
    }
    None
}

fn remove_element(html: &str, tag: &str) -> String {
    let lower = html.to_ascii_lowercase();
    let open = format!("<{tag}");
    let close_prefix = format!("</{tag}");
    let mut out = String::with_capacity(html.len());
    let mut i = 0;
    while i < html.len() {
        if lower[i..].starts_with(&open) {
            let boundary = lower[i + open.len()..].chars().next();
            let is_tag = matches!(boundary, Some(' ' | '\t' | '\n' | '\r' | '>' | '/') | None);
            if is_tag {
                match find_close_tag_end(&lower[i..], &close_prefix) {
                    Some(rel) => {
                        i += rel;
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

const DANGER_SCHEMES: &[&str] = &[
    "javascript:",
    "vbscript:",
    "data:text/html",
    "data:image/svg+xml",
];

/// リンクターゲットが始まる位置の区切り。戻り値は (区切りのバイト長, 直後の空白を読み飛ばすか)。
/// オートリンク`<`で空白を読み飛ばさないのは、`< javascript:`がオートリンクではないため。
fn link_delimiter_at(bytes: &[u8], i: usize) -> Option<(usize, bool)> {
    match bytes[i] {
        b'<' => Some((1, false)),
        b']' if matches!(bytes.get(i + 1), Some(b'(' | b':')) => Some((2, true)),
        _ => None,
    }
}

/// 制御文字（Cc）を読み飛ばして`scheme`に前方一致するか。C0・DEL・C1は出力段の
/// `output::strip_terminal_controls`が削除し、タブと改行はURLの解釈時に無視される。判定が
/// これらで途切れると、`](\x01javascript:`が無害化をすり抜けたあと危険リンクへ戻る。
/// 最初の不一致で打ち切るため、走査量は入力全体で線形に収まる。
fn matches_scheme_ignoring_controls(s: &str, scheme: &str) -> bool {
    let mut want = scheme.bytes().peekable();
    for c in s.chars() {
        let Some(&w) = want.peek() else {
            return true;
        };
        if c.is_control() {
            continue;
        }
        if !c.is_ascii() || (c as u8).to_ascii_lowercase() != w {
            return false;
        }
        want.next();
    }
    want.peek().is_none()
}

fn starts_with_danger_scheme(s: &str) -> bool {
    DANGER_SCHEMES
        .iter()
        .any(|d| matches_scheme_ignoring_controls(s, d))
}

/// インラインリンク`](`、参照定義`]:`、オートリンク`<`のターゲットのうち、クリックで
/// スクリプトが走りうる実行系スキームだけを`unsafe-`接頭辞で無害化する（A03）。
/// 通常のURLや`data:image/png`等の非実行データURLはそのまま残す。文字は削らないため、
/// `Mutex<T>`や生のHTMLタグ（`<a href="javascript:x">`）は変わらない。
pub fn sanitize_link_schemes(md: &str) -> String {
    let bytes = md.as_bytes();
    let mut out = String::with_capacity(md.len());
    let mut i = 0;
    while i < bytes.len() {
        let Some((delim_len, skip_ws)) = link_delimiter_at(bytes, i) else {
            // 区切り以外は1文字ずつ写す。区切りと空白はASCIIなのでiは常にchar境界に乗る。
            let ch = md[i..].chars().next().unwrap();
            out.push(ch);
            i += ch.len_utf8();
            continue;
        };
        out.push_str(&md[i..i + delim_len]);
        i += delim_len;
        if skip_ws {
            let ws = bytes[i..]
                .iter()
                .take_while(|b| matches!(b, b' ' | b'\t' | b'\n' | b'\r'))
                .count();
            out.push_str(&md[i..i + ws]);
            i += ws;
        }
        if starts_with_danger_scheme(&md[i..]) {
            out.push_str("unsafe-");
        }
    }
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

/// `<`で始まる断片のタグ終端`>`の直後位置を返す。引用符（`"`/`'`）の内側の`>`は
/// 属性値の一部なのでタグを閉じない。終端が無ければNone。
/// 引用に入るのは`=`の直後（間の空白は許す）に現れた引用符だけとする。
/// 非引用の属性値に含まれるアポストロフィ（`data-x=it's`）を開始引用と誤認しないため。
fn find_tag_end(s: &str) -> Option<usize> {
    let mut quote: Option<char> = None;
    let mut prev_sig: Option<char> = None;
    for (idx, c) in s.char_indices().skip(1) {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None => match c {
                '"' | '\'' if prev_sig == Some('=') => quote = Some(c),
                '>' => return Some(idx + 1),
                _ => {}
            },
        }
        if !c.is_whitespace() {
            prev_sig = Some(c);
        }
    }
    None
}

/// 可視テキストの文字数（Unicodeスカラー値）。エスカレーション判定と`static_chars`/`rendered_chars`に使う。
/// script/style/noscriptを要素ごと除去し、タグを落とし、代表的な実体参照を1文字に戻し、
/// 空白を畳んでtrimする。リンク先や画像URLは含まない。失敗しない。
/// タグ境界に空白を挿入して、隣接する要素のテキストが単語として混在しないようにする。
pub fn visible_text_len(html: &str) -> usize {
    let stripped = strip_non_content(html);
    let mut text = String::with_capacity(stripped.len());
    let mut i = 0usize;
    while i < stripped.len() {
        let rest = &stripped[i..];
        if let Some(inner) = rest.strip_prefix("<!--") {
            // コメントは中身ごと落とす。終端が無ければ以降すべてコメント扱い。
            i += match inner.find("-->") {
                Some(p) => 4 + p + 3,
                None => rest.len(),
            };
            continue;
        }
        if rest.starts_with('<') {
            match find_tag_end(rest) {
                Some(end) => {
                    text.push(' ');
                    i += end;
                    continue;
                }
                // 終端`>`が無い`<`はタグではなく本文。以降を切り捨てない。
                None => {
                    text.push_str(rest);
                    break;
                }
            }
        }
        let ch = rest.chars().next().unwrap();
        text.push(ch);
        i += ch.len_utf8();
    }
    let text = text
        .replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&");
    let trimmed = text.trim();
    let mut count = 0usize;
    let mut last_was_space = true;
    for c in trimmed.chars() {
        if c.is_whitespace() {
            if !last_was_space {
                count += 1;
                last_was_space = true;
            }
        } else {
            count += 1;
            last_was_space = false;
        }
    }
    count
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
    fn sanitize_link_schemes_covers_three_forms() {
        for (input, want) in [
            ("[x](javascript:a)", "[x](unsafe-javascript:a)"),
            ("[x](  javascript:a)", "[x](  unsafe-javascript:a)"),
            ("<javascript:a>", "<unsafe-javascript:a>"),
            ("[x]: javascript:a", "[x]: unsafe-javascript:a"),
            ("[x]:\n  javascript:a", "[x]:\n  unsafe-javascript:a"),
            ("[x](JavaScript:a)", "[x](unsafe-JavaScript:a)"),
            ("<VBScript:a>", "<unsafe-VBScript:a>"),
            ("[x](data:text/html,y)", "[x](unsafe-data:text/html,y)"),
            (
                "[x](data:image/svg+xml,y)",
                "[x](unsafe-data:image/svg+xml,y)",
            ),
        ] {
            assert_eq!(sanitize_link_schemes(input), want, "input={input:?}");
        }
    }

    #[test]
    fn sanitize_link_schemes_sees_through_control_characters() {
        // 制御文字は出力段が削除し、タブと改行はURLの解釈時に無視される。判定が
        // これらで途切れると、無害化をすり抜けた文字列が出力時に危険リンクへ戻る。
        for (input, want) in [
            ("[x](\u{1}javascript:a)", "[x](unsafe-\u{1}javascript:a)"),
            ("<\u{1}javascript:a>", "<unsafe-\u{1}javascript:a>"),
            ("[x]: \u{1}javascript:a", "[x]: unsafe-\u{1}javascript:a"),
            ("[x](java\tscript:a)", "[x](unsafe-java\tscript:a)"),
            ("[x](java\nscript:a)", "[x](unsafe-java\nscript:a)"),
            (
                "[x](j\u{7f}a\u{85}vascript\u{1b}:a)",
                "[x](unsafe-j\u{7f}a\u{85}vascript\u{1b}:a)",
            ),
            ("<data\u{0}:text/html,y>", "<unsafe-data\u{0}:text/html,y>"),
        ] {
            assert_eq!(sanitize_link_schemes(input), want, "input={input:?}");
        }
        // 制御文字だけで危険スキームにならないものは変えない。
        for s in ["[x](\u{1}https://ok.test/)", "<\u{1}T>", "[x](java\u{1}"] {
            assert_eq!(sanitize_link_schemes(s), s, "input={s:?}");
        }
    }

    #[test]
    fn sanitize_link_schemes_keeps_everything_else_byte_identical() {
        for s in [
            "[x](https://ok.test/p)",
            "[x](data:image/png;base64,AAAA)",
            "struct G<T: ?Sized> { inner: Mutex<T> }",
            r#"<a href="javascript:x">y</a>"#,
            "<script>var s='javascript:a';</script>",
            "a < b > c",
            "[x]: javascript-ish",
            "[x]javascript:a",
            "[x] javascript:a",
            "日本語のテキスト <T> です",
        ] {
            assert_eq!(sanitize_link_schemes(s), s, "input={s:?}");
        }
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
    fn visible_text_len_handles_closing_tag_with_space_before_gt() {
        // `</script >` のように閉じタグの `>` 前に空白があっても正当なHTML。
        // 誤って未終端扱いすると以降の本文が丸ごと落ちる回帰ガード。
        let html = "<script>x</script ><article><p>body</p></article>";
        assert_eq!(visible_text_len(html), 4);
    }

    #[test]
    fn visible_text_len_handles_closing_tag_with_newline_before_gt() {
        let html = "<script>x</SCRIPT\n><article><p>body</p></article>";
        assert_eq!(visible_text_len(html), 4);
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

    #[test]
    fn visible_text_len_ignores_urls_and_scripts() {
        // 30文字超のhrefを持つリンク10個。アンカーテキスト3文字×10と、リンク間の境界空白9つ = 39。hrefは数えない。
        let nav: String = (0..10)
            .map(|i| format!("<a href=\"https://example.com/very/long/path/segment/{i:04}/page.html\">ホーム</a>"))
            .collect();
        let html = format!(
            "<html><head><style>p{{}}</style><script>var x='xxxxxxxxxx';</script></head><body><nav>{nav}</nav><div id=\"app\"></div></body></html>"
        );
        assert_eq!(visible_text_len(&html), 39);
    }

    #[test]
    fn visible_text_len_counts_article_text_and_entities() {
        let html = "<article><h1>見出し</h1><p>本文&amp;続き&nbsp;末尾</p></article>";
        // 見出し(3) + タグ境界(1) + 本文&続き 末尾(8) = 12。
        assert_eq!(visible_text_len(html), 12);
        assert_eq!(visible_text_len(""), 0);
        assert_eq!(visible_text_len("<div id=\"app\"></div>"), 0);
        // 二重エスケープ: &amp;lt; は &lt; であって < ではない（4文字）。
        assert_eq!(visible_text_len("<p>&amp;lt;</p>"), 4);
    }

    #[test]
    fn visible_text_len_tag_scanner_edge_cases() {
        // 引用符内の`>`はタグを閉じない。
        assert_eq!(visible_text_len("<a title=\">\">x</a>"), 1);
        assert_eq!(visible_text_len("<a title='>'>x</a>"), 1);
        // 非引用の属性値のアポストロフィは開始引用ではない。
        assert_eq!(visible_text_len("<div data-x=it's>text</div>"), 4);
        // `=`と引用符の間の空白は許す。
        assert_eq!(visible_text_len("<a title= \">\">x</a>"), 1);
        // 終端`>`の無い`<`は本文。以降も落とさない。
        assert_eq!(visible_text_len("a < b"), 5);
        assert_eq!(visible_text_len("<p>x<"), 2);
        // コメントは中身ごと除去する。
        assert_eq!(visible_text_len("<p>x</p><!-- <b>y</b> -->"), 1);
        // 終端の無いコメントは以降すべて除去。
        assert_eq!(visible_text_len("<p>x</p><!-- y"), 1);
    }
}
