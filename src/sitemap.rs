//! `--map`: sitemapと起点ページのリンクから、起点と同じホストのURL一覧を作る。

use crate::error::{WebgrabError, sanitize_detail};
use crate::fetch::{self, FetchOptions};
use std::collections::{HashSet, VecDeque};
use std::time::Instant;
use url::Url;

/// 取得するsitemapの上限（sitemap indexそのものも1つと数える）。
pub const MAX_SITEMAPS: usize = 20;

/// `start`のホストのsitemapから`<loc>`のURLを`list`へ入れる。sitemapの失敗は警告にして続け、
/// 期限・`bytes_left`・件数の上限に達したら残りを打ち切る。
pub async fn collect(
    start: &str,
    base: &FetchOptions,
    deadline: Instant,
    bytes_left: u64,
    list: &mut UrlList,
) {
    let Ok(start) = Url::parse(start) else {
        return;
    };
    let mut budget = Budget {
        deadline,
        bytes_left,
    };
    let mut pending = Pending::default();
    for url in initial_sitemaps(&start, &mut budget, base).await {
        pending.offer(url, false, 0);
    }
    let mut fetched = 0;
    let mut stopped = false;
    while let Some((url, nested)) = pending.queue.pop_front() {
        if let Some(reason) = budget.stop_reason() {
            warn_skipped(reason, pending.remaining() + 1);
            stopped = true;
            break;
        }
        fetched += 1;
        let same_origin = url.origin() == start.origin();
        match budget.get(&url, base, base.check_robots, same_origin).await {
            Ok(xml) => {
                for child in classify(&xml, nested, list) {
                    pending.offer(child, true, fetched);
                }
            }
            Err(e) => warn_failed(&url, &e),
        }
    }
    if !stopped && pending.dropped > 0 {
        warn_skipped("limit", pending.dropped);
    }
    if pending.gzip > 0 {
        warn_skipped("gzip", pending.gzip);
    }
}

fn warn_skipped(reason: &str, remaining: usize) {
    eprintln!("webgrab: warn=sitemap-skipped reason={reason} remaining={remaining}");
}

/// 取得を待つsitemap。件数の上限に入りきらない分と読まないgzipは数だけ持つ（巨大なindexの子を
/// すべて抱えず、警告も1行にまとめるため）。
#[derive(Default)]
struct Pending {
    queue: VecDeque<(Url, bool)>,
    dropped: usize,
    gzip: usize,
}

impl Pending {
    fn offer(&mut self, url: Url, nested: bool, fetched: usize) {
        if url.path().to_ascii_lowercase().ends_with(".gz") {
            self.gzip += 1;
        } else if fetched + self.queue.len() >= MAX_SITEMAPS {
            self.dropped += 1;
        } else {
            self.queue.push_back((url, nested));
        }
    }

    fn remaining(&self) -> usize {
        self.queue.len() + self.dropped
    }
}

/// robots.txtの`Sitemap:`行が指すsitemap。robots.txtはこの行を読むためだけに取得する
/// （robots.txt自身はrobotsの判定対象にしない）。行が無いか読めなければ`/sitemap.xml`を見る。
async fn initial_sitemaps(start: &Url, budget: &mut Budget, base: &FetchOptions) -> Vec<Url> {
    let mut declared = Vec::new();
    if let Ok(robots_url) = start.join("/robots.txt")
        && let Ok(text) = budget.get(&robots_url, base, false, true).await
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

/// sitemapの本文を読む。urlsetのURLは`list`へ入れ、indexなら子sitemapを返す。indexの子が
/// またindexならたどらない（1段だけ）。`<loc>`は完全なURLだけを読む（sitemaps.orgの規約）。
/// 相対URLを解決すると、長いsitemapのURLを基準に短い`<loc>`が何倍にも膨らみ、
/// `--max-bytes`に収まる入力から際限なくメモリを使わせられる。
fn classify<'a>(xml: &'a str, nested: bool, list: &mut UrlList) -> impl Iterator<Item = Url> + 'a {
    let resolved = locs(xml).filter_map(|l| Url::parse(&l).ok());
    let children = match kind(xml) {
        Kind::Index if nested => {
            eprintln!("webgrab: warn=sitemap-skipped reason=nested-index");
            None
        }
        Kind::Index => Some(resolved),
        Kind::UrlSet => {
            for u in resolved {
                list.push(u.as_str());
            }
            None
        }
    };
    children.into_iter().flatten()
}

/// 全体の期限と`--max-bytes`の残り。
struct Budget {
    deadline: Instant,
    bytes_left: u64,
}

impl Budget {
    /// 次のsitemapを取得してはいけない理由。
    fn stop_reason(&self) -> Option<&'static str> {
        if self.bytes_left == 0 {
            Some("max-bytes")
        } else if Instant::now() >= self.deadline {
            Some("timeout")
        } else {
            None
        }
    }

    /// 残りの期限とバイト数の範囲で`url`を取得し、UTF-8として読む（sitemapの規約はUTF-8）。
    /// `--allow-private`は利用者が起点に与えたものなので、起点と別オリジンの取得には渡さない。
    /// robots.txtやsitemap indexが内部アドレスを指しても、そこへは要求を出さない。
    async fn get(
        &mut self,
        url: &Url,
        base: &FetchOptions,
        check_robots: bool,
        same_origin: bool,
    ) -> Result<String, WebgrabError> {
        let opts = FetchOptions {
            user_agent: base.user_agent.clone(),
            timeout: self.deadline.saturating_duration_since(Instant::now()),
            max_bytes: self.bytes_left,
            allow_private: base.allow_private && same_origin,
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

/// sitemapの種類を見る。
pub fn kind(xml: &str) -> Kind {
    if xml.contains("<sitemapindex") {
        Kind::Index
    } else {
        Kind::UrlSet
    }
}

/// sitemapのXMLから`<loc>`の値を出現順に1件ずつ取り出す。XMLパーサーは使わず`<loc>`だけを
/// 前から1回走査する（巨大・深い入れ子の文書でも線形時間で終わり、全件を同時には持たない）。
pub fn locs(xml: &str) -> impl Iterator<Item = String> + '_ {
    let mut rest = xml;
    std::iter::from_fn(move || {
        loop {
            let (value, after) = next_loc(rest)?;
            rest = after;
            if value.is_some() {
                return value;
            }
        }
    })
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

/// `;`を探す範囲（`&`と`;`を含む）。最長の参照`&#x10FFFF;`の長さ。区切らないと`&`だけが
/// 大量に並ぶ入力で`;`探しが末尾まで繰り返され、二乗時間になる。
const MAX_REF_LEN: usize = 10;

/// XMLの定義済み実体参照5つと文字参照（`&#38;`、`&#x26;`）を前から1回で戻す。戻した文字は
/// 再び解釈しない（`&amp;lt;`は`&lt;`になる）。解釈できない参照はそのまま残す。
fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        let tail = &rest[at..];
        let window = &tail.as_bytes()[..tail.len().min(MAX_REF_LEN)];
        let decoded = window
            .iter()
            .position(|&b| b == b';')
            .and_then(|end| Some((reference(&tail[1..end])?, end)));
        match decoded {
            Some((c, end)) => {
                out.push(c);
                rest = &tail[end + 1..];
            }
            None => {
                out.push('&');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// `&`と`;`の間の参照名を文字にする（XML 1.0 §4.1。16進は小文字の`x`だけ）。
fn reference(name: &str) -> Option<char> {
    let (digits, radix) = match name {
        "lt" => return Some('<'),
        "gt" => return Some('>'),
        "quot" => return Some('"'),
        "apos" => return Some('\''),
        "amp" => return Some('&'),
        _ => match name.strip_prefix("#x") {
            Some(hex) => (hex, 16),
            None => (name.strip_prefix('#')?, 10),
        },
    };
    // from_str_radixは先頭の`+`を受けるため、桁だけで出来ているかを先に見る。
    if digits.is_empty() || !digits.chars().all(|c| c.is_digit(radix)) {
        return None;
    }
    let c = char::from_u32(u32::from_str_radix(digits, radix).ok()?)?;
    (c != '\0').then_some(c)
}

/// `start`と同じホストのhttp/httpsのURLを、fragment除去・重複除去して受け取った順に並べる。
/// 候補は1件ずつ受け、条件に合うものだけを持つ。
pub struct UrlList {
    host: Option<String>,
    seen: HashSet<String>,
    out: String,
}

impl UrlList {
    pub fn new(start: &str) -> Self {
        let host = Url::parse(start)
            .ok()
            .and_then(|u| u.host_str().map(str::to_string));
        Self {
            host,
            seen: HashSet::new(),
            out: String::new(),
        }
    }

    pub fn push(&mut self, candidate: &str) {
        let Some(host) = self.host.as_deref() else {
            return;
        };
        let Some(url) = crate::links::resolve(None, candidate) else {
            return;
        };
        let same = Url::parse(&url).is_ok_and(|u| u.host_str() == Some(host));
        if !same || self.seen.contains(&url) {
            return;
        }
        if !self.out.is_empty() {
            self.out.push('\n');
        }
        self.out.push_str(&url);
        self.seen.insert(url);
    }

    /// 1行1件の一覧。
    pub fn finish(self) -> String {
        self.out
    }
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

    fn parse(xml: &str) -> (Kind, Vec<String>) {
        (kind(xml), locs(xml).collect())
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
    fn unescape_reads_references_up_to_the_longest_code_point_only() {
        assert_eq!(unescape("&#x10FFFF;"), "\u{10FFFF}");
        assert_eq!(unescape("&#x110000;&#x010FFFF;"), "&#x110000;&#x010FFFF;");
        assert_eq!(unescape("&&;&#;&#x;&#+1;&amp"), "&&;&#;&#x;&#+1;&amp");
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
    fn parse_unescapes_numeric_character_references() {
        let xml = "<urlset><loc>https://x.test/?a=1&#38;b=2&#x26;c=&#X3C;&#0;&#xZZ;&#99999999;</loc></urlset>";
        assert_eq!(
            parse(xml).1,
            vec!["https://x.test/?a=1&b=2&c=&#X3C;&#0;&#xZZ;&#99999999;".to_string()]
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

    fn same_host_list(start: &str, candidates: &[&str]) -> String {
        let mut list = UrlList::new(start);
        for c in candidates {
            list.push(c);
        }
        list.finish()
    }

    #[test]
    fn url_list_keeps_same_host_http_urls_deduplicated_in_order() {
        let got = same_host_list(
            "https://x.test/start",
            &[
                "https://x.test/b#frag",
                "http://x.test/a",
                "https://x.test/b",
                "https://other.test/c",
                "https://sub.x.test/d",
                "mailto:a@x.test",
                "relative/path",
            ],
        );
        assert_eq!(got, "https://x.test/b\nhttp://x.test/a");
    }

    #[test]
    fn url_list_is_empty_for_unparsable_start() {
        assert_eq!(same_host_list("not a url", &["https://x.test/a"]), "");
    }

    #[test]
    fn classify_reads_only_absolute_locs() {
        let mut list = UrlList::new("https://x.test/");
        let idx = "<sitemapindex><sitemap><loc>c1.xml</loc></sitemap><sitemap><loc>https://x.test/maps/c2.xml</loc></sitemap></sitemapindex>";
        assert_eq!(
            classify(idx, false, &mut list).collect::<Vec<_>>(),
            vec![Url::parse("https://x.test/maps/c2.xml").unwrap()]
        );
        let set =
            "<urlset><loc>/p</loc><loc>https://y.test/q</loc><loc>https://x.test/r</loc></urlset>";
        assert_eq!(classify(set, true, &mut list).count(), 0);
        assert_eq!(list.finish(), "https://x.test/r");
    }

    #[test]
    fn classify_does_not_follow_an_index_inside_an_index() {
        let mut list = UrlList::new("https://x.test/");
        let idx = "<sitemapindex><sitemap><loc>https://x.test/c.xml</loc></sitemap></sitemapindex>";
        assert_eq!(classify(idx, true, &mut list).count(), 0);
        assert_eq!(list.finish(), "");
    }

    #[test]
    fn pending_holds_up_to_the_limit_and_counts_the_rest() {
        let u = |p: &str| Url::parse(&format!("https://x.test/{p}")).unwrap();
        let mut pending = Pending::default();
        pending.offer(u("a.xml.GZ"), false, 0);
        assert_eq!((pending.remaining(), pending.gzip), (0, 1));
        for i in 0..MAX_SITEMAPS {
            pending.offer(u(&format!("{i}.xml")), true, 1);
        }
        assert_eq!(pending.queue.len(), MAX_SITEMAPS - 1);
        assert_eq!(pending.dropped, 1);
        assert_eq!(pending.remaining(), MAX_SITEMAPS);
        assert_eq!(pending.queue.front(), Some(&(u("0.xml"), true)));
    }

    #[test]
    fn stop_reason_checks_bytes_then_deadline() {
        let later = Instant::now() + std::time::Duration::from_secs(60);
        let b = |bytes_left, deadline| Budget {
            deadline,
            bytes_left,
        };
        assert_eq!(b(1, later).stop_reason(), None);
        assert_eq!(b(0, later).stop_reason(), Some("max-bytes"));
        let past = Instant::now();
        assert_eq!(b(0, past).stop_reason(), Some("max-bytes"));
        assert_eq!(b(1, past).stop_reason(), Some("timeout"));
    }
}
