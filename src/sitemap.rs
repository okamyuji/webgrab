//! `--map`: sitemapと起点ページのリンクから、起点と同じホストのURL一覧を作る。

use crate::error::{WebgrabError, sanitize_detail};
use crate::fetch::{self, FetchOptions};
use std::collections::{HashSet, VecDeque};
use std::time::Instant;
use url::Url;

/// 取得するsitemapの上限（sitemap indexそのものも1つと数える）。
pub const MAX_SITEMAPS: usize = 20;

/// `start`のホストのsitemapから`<loc>`のURLを集める。sitemapの失敗は警告にして続け、
/// 期限・`bytes_left`・件数の上限に達したら残りを打ち切る。
pub async fn collect(
    start: &str,
    base: &FetchOptions,
    deadline: Instant,
    bytes_left: u64,
) -> Vec<String> {
    let Ok(start) = Url::parse(start) else {
        return Vec::new();
    };
    let mut budget = Budget {
        deadline,
        bytes_left,
    };
    let first = initial_sitemaps(&start, &mut budget, base).await;
    let mut queue: VecDeque<(Url, bool)> = first.into_iter().map(|u| (u, false)).collect();
    let mut urls = Vec::new();
    let mut fetched = 0;
    while let Some((url, nested)) = queue.pop_front() {
        if let Some(reason) = budget.stop_reason(fetched) {
            eprintln!(
                "webgrab: warn=sitemap-skipped reason={reason} remaining={}",
                queue.len() + 1
            );
            break;
        }
        if url.path().to_ascii_lowercase().ends_with(".gz") {
            eprintln!("webgrab: warn=sitemap-skipped reason=gzip");
            continue;
        }
        fetched += 1;
        match budget.get(&url, base, base.check_robots).await {
            Ok(xml) => match classify(&url, &xml, nested) {
                Found::Children(c) => queue.extend(c.into_iter().map(|u| (u, true))),
                Found::Urls(u) => urls.extend(u),
            },
            Err(e) => warn_failed(&url, &e),
        }
    }
    urls
}

/// robots.txtの`Sitemap:`行が指すsitemap。robots.txtはこの行を読むためだけに取得する
/// （robots.txt自身はrobotsの判定対象にしない）。行が無いか読めなければ`/sitemap.xml`を見る。
async fn initial_sitemaps(start: &Url, budget: &mut Budget, base: &FetchOptions) -> Vec<Url> {
    let mut declared = Vec::new();
    if let Ok(robots_url) = start.join("/robots.txt")
        && let Ok(text) = budget.get(&robots_url, base, false).await
    {
        declared = robots_sitemaps(&text)
            .iter()
            .filter_map(|s| robots_url.join(s).ok())
            .collect();
    }
    if declared.is_empty() {
        declared.extend(start.join("/sitemap.xml").ok());
    }
    declared
}

/// 1つのsitemapから得たもの。
#[derive(Debug, PartialEq, Eq)]
enum Found {
    /// sitemap indexが指す子sitemap。
    Children(Vec<Url>),
    /// ページのURL。
    Urls(Vec<String>),
}

/// sitemapの本文を分類する。`<loc>`は`url`基準で解決する。indexの子がまたindexなら
/// たどらない（1段だけ）。
fn classify(url: &Url, xml: &str, nested: bool) -> Found {
    let (kind, locs) = parse(xml);
    let resolved = locs.iter().filter_map(|l| url.join(l).ok());
    match kind {
        Kind::Index if nested => {
            eprintln!("webgrab: warn=sitemap-skipped reason=nested-index");
            Found::Urls(Vec::new())
        }
        Kind::Index => Found::Children(resolved.collect()),
        Kind::UrlSet => Found::Urls(resolved.map(String::from).collect()),
    }
}

/// 全体の期限と`--max-bytes`の残り。
struct Budget {
    deadline: Instant,
    bytes_left: u64,
}

impl Budget {
    /// 次のsitemapを取得してはいけない理由。
    fn stop_reason(&self, fetched: usize) -> Option<&'static str> {
        if fetched >= MAX_SITEMAPS {
            Some("limit")
        } else if self.bytes_left == 0 {
            Some("max-bytes")
        } else if Instant::now() >= self.deadline {
            Some("timeout")
        } else {
            None
        }
    }

    /// 残りの期限とバイト数の範囲で`url`を取得し、UTF-8として読む（sitemapの規約はUTF-8）。
    async fn get(
        &mut self,
        url: &Url,
        base: &FetchOptions,
        check_robots: bool,
    ) -> Result<String, WebgrabError> {
        let opts = FetchOptions {
            user_agent: base.user_agent.clone(),
            timeout: self.deadline.saturating_duration_since(Instant::now()),
            max_bytes: self.bytes_left,
            allow_private: base.allow_private,
            check_robots,
            allow_xml: true,
        };
        match fetch::fetch(url.as_str(), &opts).await {
            Ok(f) => {
                self.bytes_left = self.bytes_left.saturating_sub(f.consumed_bytes);
                Ok(String::from_utf8_lossy(&f.body).into_owned())
            }
            Err(e) => {
                // 上限を超えた取得は残りをすべて読み終えている。次の取得に残りは無い。
                if e.message.starts_with("response exceeds --max-bytes") {
                    self.bytes_left = 0;
                }
                Err(e)
            }
        }
    }
}

/// `warn=auto-render-failed`と同じく、1行目に機械可読の印、2行目に理由とURLを出す。
fn warn_failed(url: &Url, e: &WebgrabError) {
    eprintln!("webgrab: warn=sitemap-failed");
    eprintln!("{}", sanitize_detail(&format!("{} url={url}", e.message)));
}

/// robots.txtの`Sitemap:`行の値を出現順に返す。
pub fn robots_sitemaps(robots: &str) -> Vec<String> {
    robots
        .lines()
        .filter_map(|line| {
            let (key, value) = line.split_once(':')?;
            let value = value.trim();
            (key.trim().eq_ignore_ascii_case("sitemap") && !value.is_empty())
                .then(|| value.to_string())
        })
        .collect()
}

/// sitemapの種類。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    UrlSet,
    Index,
}

/// sitemapのXMLから`<loc>`の値を出現順に取り出す。XMLパーサーは使わず`<loc>`だけを
/// 前から1回走査する（巨大・深い入れ子の文書でも線形時間で終わる）。
pub fn parse(xml: &str) -> (Kind, Vec<String>) {
    let kind = if xml.contains("<sitemapindex") {
        Kind::Index
    } else {
        Kind::UrlSet
    };
    let mut locs = Vec::new();
    let mut rest = xml;
    while let Some((value, after)) = next_loc(rest) {
        if let Some(v) = value {
            locs.push(v);
        }
        rest = after;
    }
    (kind, locs)
}

/// 次の`<loc>`要素を探す。`(値, 残り)`を返し、値が空なら`None`を入れる。
/// 閉じタグが無ければ走査を終える。
fn next_loc(xml: &str) -> Option<(Option<String>, &str)> {
    let mut rest = xml;
    loop {
        let at = rest.find("<loc")?;
        let after_name = &rest[at + 4..];
        // `<location>`のような別名のタグを除く。
        if !after_name.starts_with(|c: char| c == '>' || c.is_ascii_whitespace()) {
            rest = after_name;
            continue;
        }
        let open_end = after_name.find('>')?;
        let body = &after_name[open_end + 1..];
        let close = body.find("</loc")?;
        return Some((loc_value(&body[..close]), &body[close..]));
    }
}

/// `<loc>`の中身をURL文字列にする。CDATAは中身をそのまま使い、それ以外は実体参照を戻す。
fn loc_value(raw: &str) -> Option<String> {
    let t = raw.trim();
    let v = match t
        .strip_prefix("<![CDATA[")
        .and_then(|s| s.strip_suffix("]]>"))
    {
        Some(cdata) => cdata.trim().to_string(),
        None => unescape(t),
    };
    (!v.is_empty()).then_some(v)
}

/// XMLの定義済み実体参照5つを戻す。`&amp;`を最後に戻し、`&amp;lt;`を`<`にしない。
fn unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// 候補URLのうち`start`と同じホストのhttp/httpsだけを、fragment除去・重複除去して1行1件に並べる。
pub fn same_host_list(start: &str, candidates: impl IntoIterator<Item = String>) -> String {
    let Some(host) = Url::parse(start)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
    else {
        return String::new();
    };
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for c in candidates {
        let Some(url) = crate::links::resolve(None, &c) else {
            continue;
        };
        let same = Url::parse(&url).is_ok_and(|u| u.host_str() == Some(host.as_str()));
        if same && seen.insert(url.clone()) {
            out.push(url);
        }
    }
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn robots_sitemaps_reads_every_sitemap_line_case_insensitively() {
        let txt = "User-agent: *\nDisallow: /a\nSitemap: https://x.test/s1.xml\nsitemap:https://x.test/s2.xml  \r\nSITEMAP : https://cdn.test/s3.xml\n";
        assert_eq!(
            robots_sitemaps(txt),
            vec![
                "https://x.test/s1.xml",
                "https://x.test/s2.xml",
                "https://cdn.test/s3.xml"
            ]
        );
    }

    #[test]
    fn robots_sitemaps_ignores_empty_values_and_other_keys() {
        let txt =
            "Sitemap:\nSitemaps: https://x.test/no.xml\nDisallow: /sitemap.xml\nno colon line\n";
        assert!(robots_sitemaps(txt).is_empty());
    }

    #[test]
    fn parse_urlset_returns_locs_in_order() {
        let xml = r#"<?xml version="1.0"?><urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">
<url><loc>https://x.test/a</loc><lastmod>2026-01-01</lastmod></url>
<url><loc>
  https://x.test/b
</loc></url></urlset>"#;
        assert_eq!(
            parse(xml),
            (
                Kind::UrlSet,
                vec!["https://x.test/a".into(), "https://x.test/b".into()]
            )
        );
    }

    #[test]
    fn parse_detects_sitemap_index() {
        let xml =
            "<sitemapindex><sitemap><loc>https://x.test/s1.xml</loc></sitemap></sitemapindex>";
        assert_eq!(
            parse(xml),
            (Kind::Index, vec!["https://x.test/s1.xml".into()])
        );
    }

    #[test]
    fn parse_unescapes_the_five_xml_entities() {
        let xml =
            "<urlset><url><loc>https://x.test/?a=1&amp;b=&lt;&gt;&quot;&apos;</loc></url></urlset>";
        assert_eq!(
            parse(xml).1,
            vec!["https://x.test/?a=1&b=<>\"'".to_string()]
        );
    }

    #[test]
    fn parse_unwraps_cdata_without_unescaping_it() {
        let xml = "<urlset><url><loc><![CDATA[https://x.test/?a=1&amp;b]]></loc></url></urlset>";
        assert_eq!(parse(xml).1, vec!["https://x.test/?a=1&amp;b".to_string()]);
    }

    #[test]
    fn parse_skips_tags_that_only_start_with_loc() {
        let xml = "<urlset><url><location>https://x.test/no</location><loc a=\"1\">https://x.test/yes</loc></url></urlset>";
        assert_eq!(parse(xml).1, vec!["https://x.test/yes".to_string()]);
    }

    #[test]
    fn parse_drops_unterminated_and_empty_locs() {
        assert!(parse("<urlset><loc></loc><loc>  </loc>").1.is_empty());
        assert!(parse("<urlset><loc>https://x.test/cut").1.is_empty());
        assert!(parse("<urlset><loc").1.is_empty());
    }

    #[test]
    fn same_host_list_keeps_same_host_http_urls_deduplicated_in_order() {
        let got = same_host_list(
            "https://x.test/start",
            [
                "https://x.test/b#frag".to_string(),
                "http://x.test/a".to_string(),
                "https://x.test/b".to_string(),
                "https://other.test/c".to_string(),
                "https://sub.x.test/d".to_string(),
                "mailto:a@x.test".to_string(),
                "relative/path".to_string(),
            ],
        );
        assert_eq!(got, "https://x.test/b\nhttp://x.test/a");
    }

    #[test]
    fn classify_resolves_locs_against_the_sitemap_url() {
        let base = Url::parse("https://x.test/maps/s.xml").unwrap();
        let idx = "<sitemapindex><sitemap><loc>c1.xml</loc></sitemap></sitemapindex>";
        assert_eq!(
            classify(&base, idx, false),
            Found::Children(vec![Url::parse("https://x.test/maps/c1.xml").unwrap()])
        );
        assert_eq!(
            classify(
                &base,
                "<urlset><loc>/p</loc><loc>https://y.test/q</loc></urlset>",
                true
            ),
            Found::Urls(vec!["https://x.test/p".into(), "https://y.test/q".into()])
        );
    }

    #[test]
    fn classify_does_not_follow_an_index_inside_an_index() {
        let base = Url::parse("https://x.test/s.xml").unwrap();
        let idx = "<sitemapindex><sitemap><loc>/c.xml</loc></sitemap></sitemapindex>";
        assert_eq!(classify(&base, idx, true), Found::Urls(Vec::new()));
    }

    #[test]
    fn stop_reason_checks_limit_then_bytes_then_deadline() {
        let later = Instant::now() + std::time::Duration::from_secs(60);
        let b = |bytes_left, deadline| Budget {
            deadline,
            bytes_left,
        };
        assert_eq!(b(1, later).stop_reason(MAX_SITEMAPS - 1), None);
        assert_eq!(b(1, later).stop_reason(MAX_SITEMAPS), Some("limit"));
        assert_eq!(b(0, later).stop_reason(MAX_SITEMAPS), Some("limit"));
        assert_eq!(b(0, later).stop_reason(0), Some("max-bytes"));
        let past = Instant::now();
        assert_eq!(b(0, past).stop_reason(0), Some("max-bytes"));
        assert_eq!(b(1, past).stop_reason(0), Some("timeout"));
    }

    #[test]
    fn same_host_list_is_empty_for_unparsable_start() {
        assert_eq!(
            same_host_list("not a url", ["https://x.test/a".to_string()]),
            ""
        );
    }
}
