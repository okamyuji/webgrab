//! `--format links`: HTML内の`<a href>`を絶対URLの一覧にする。

use dom_query::Document;
use std::collections::HashSet;
use url::Url;

/// `base`基準で解決した http/https のリンクを、初出順・fragment除去・重複除去で1行1件に並べる。
pub fn extract_links(html: &str, base: &str) -> String {
    let base = Url::parse(base).ok();
    let doc = Document::from(html);
    let mut seen = HashSet::new();
    let mut out: Vec<String> = Vec::new();
    for a in doc.select("a[href]").iter() {
        let Some(href) = a.attr("href") else { continue };
        let Some(url) = resolve(base.as_ref(), &href) else {
            continue;
        };
        if seen.insert(url.clone()) {
            out.push(url);
        }
    }
    out.join("\n")
}

/// 解決でき、http/httpsで、fragmentを除いたURL文字列。`#top`のようなページ内アンカーは
/// fragment除去後に現在のページ自身を指すだけなので捨てる。
pub(crate) fn resolve(base: Option<&Url>, href: &str) -> Option<String> {
    if href.trim_start().starts_with('#') {
        return None;
    }
    let mut url = match base {
        Some(b) => b.join(href).ok()?,
        None => Url::parse(href).ok()?,
    };
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    url.set_fragment(None);
    Some(url.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = "https://example.test/dir/page";

    fn links(html: &str) -> Vec<String> {
        extract_links(html, BASE)
            .lines()
            .map(str::to_string)
            .collect()
    }

    #[test]
    fn resolves_relative_hrefs_against_base() {
        let got = links(r#"<a href="x">a</a><a href="/y">b</a><a href="../z?q=1">c</a>"#);
        assert_eq!(
            got,
            [
                "https://example.test/dir/x",
                "https://example.test/y",
                "https://example.test/z?q=1"
            ]
        );
    }

    #[test]
    fn keeps_absolute_http_and_https() {
        let got = links(r#"<a href="http://a.test/1">a</a><a href="https://b.test/2">b</a>"#);
        assert_eq!(got, ["http://a.test/1", "https://b.test/2"]);
    }

    #[test]
    fn resolves_protocol_relative_with_base_scheme() {
        assert_eq!(
            links(r#"<a href="//cdn.test/x">a</a>"#),
            ["https://cdn.test/x"]
        );
    }

    #[test]
    fn drops_non_http_schemes_including_case_and_whitespace() {
        let html = r#"<a href="javascript:alert(1)">a</a>
            <a href="  JavaScript:alert(1)">b</a>
            <a href="JAVASCRIPT:void(0)">c</a>
            <a href="mailto:x@example.test">d</a>
            <a href="data:text/html,hi">e</a>
            <a href="tel:123">f</a>
            <a href="ftp://h.test/f">g</a>"#;
        assert!(links(html).is_empty());
    }

    #[test]
    fn trims_whitespace_around_valid_href() {
        assert_eq!(
            links("<a href=\"  /t \n\">a</a>"),
            ["https://example.test/t"]
        );
    }

    #[test]
    fn strips_fragment_and_drops_fragment_only_links() {
        let got = links(r##"<a href="#top">a</a><a href="/p#s1">b</a><a href="/p">c</a>"##);
        assert_eq!(got, ["https://example.test/p"]);
    }

    #[test]
    fn dedupes_keeping_first_appearance_order() {
        let got = links(
            r##"<a href="/b">1</a><a href="/a">2</a><a href="/b#x">3</a><a href="/a">4</a>"##,
        );
        assert_eq!(got, ["https://example.test/b", "https://example.test/a"]);
    }

    #[test]
    fn drops_unresolvable_href() {
        assert!(links(r#"<a href="http://">a</a><a href="http://[::1">b</a>"#).is_empty());
    }

    #[test]
    fn ignores_anchor_without_href() {
        assert!(links(r#"<a name="x">a</a><a>b</a>"#).is_empty());
    }

    #[test]
    fn empty_href_resolves_to_base_page() {
        assert_eq!(
            links(r#"<a href="">a</a>"#),
            ["https://example.test/dir/page"]
        );
    }

    #[test]
    fn returns_empty_when_no_anchors() {
        assert_eq!(extract_links("<p>text</p>", BASE), "");
        assert_eq!(extract_links("", BASE), "");
    }

    #[test]
    fn unparsable_base_keeps_only_absolute_links() {
        let got = extract_links(
            r#"<a href="/rel">a</a><a href="https://a.test/x">b</a>"#,
            "not a url",
        );
        assert_eq!(got, "https://a.test/x");
    }

    #[test]
    fn joins_with_newline_without_trailing_newline() {
        let got = extract_links(r#"<a href="/a">1</a><a href="/b">2</a>"#, BASE);
        assert_eq!(got, "https://example.test/a\nhttps://example.test/b");
    }

    #[test]
    fn handles_many_anchors_in_linear_time() {
        let html = "<a href=\"/p\">x</a>".repeat(100_000);
        let start = std::time::Instant::now();
        let got = links(&html);
        assert_eq!(got, ["https://example.test/p"]);
        assert!(start.elapsed().as_secs() < 10, "{:?}", start.elapsed());
    }

    #[test]
    fn handles_many_distinct_anchors() {
        let html: String = (0..100_000)
            .map(|i| format!("<a href=\"/p{i}\">x</a>"))
            .collect();
        assert_eq!(links(&html).len(), 100_000);
    }

    #[test]
    fn handles_deeply_nested_anchor_container() {
        let html = format!(
            "{}<a href=\"/d\">x</a>{}",
            "<div>".repeat(5000),
            "</div>".repeat(5000)
        );
        assert_eq!(links(&html), ["https://example.test/d"]);
    }
}
