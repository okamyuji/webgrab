//! `--map`の統合試験。ローカルの常駐サーバでrobots.txt・sitemap・起点ページを返す。

mod common;

use common::Route;
use std::process::Command;

fn run(args: &[&str]) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_webgrab"))
        .args(args)
        .output()
        .expect("binary runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

fn typed(path: &'static str, body: impl Into<String>, content_type: &'static str) -> Route {
    Route {
        content_type,
        ..Route::html(path, body)
    }
}

fn xml(path: &'static str, body: impl Into<String>) -> Route {
    typed(path, body, "application/xml")
}

fn robots(body: impl Into<String>) -> Route {
    typed("/robots.txt", body, "text/plain")
}

/// sitemapの`<loc>`は完全なURLに限るので、`/`で始まるパスは試験用サーバの`{origin}`を付ける。
fn absolute(u: &str) -> String {
    if u.starts_with('/') {
        format!("{{origin}}{u}")
    } else {
        u.to_string()
    }
}

fn urlset(urls: &[String]) -> String {
    let items: String = urls
        .iter()
        .map(|u| format!("<url><loc>{}</loc></url>", absolute(u)))
        .collect();
    format!("<?xml version=\"1.0\"?><urlset>{items}</urlset>")
}

fn index(urls: &[String]) -> String {
    let items: String = urls
        .iter()
        .map(|u| format!("<sitemap><loc>{}</loc></sitemap>", absolute(u)))
        .collect();
    format!("<sitemapindex>{items}</sitemapindex>")
}

const PAGE: &str = "<html><body><nav><a href=\"/nav\">n</a></nav><p><a href=\"/a\">dup</a> <a href=\"https://other.test/x\">o</a> <a href=\"#top\">t</a></p></body></html>";

fn lines(s: &str) -> Vec<String> {
    s.lines().map(str::to_string).collect()
}

fn ok_robots() -> Route {
    robots("User-agent: *\nAllow: /\n")
}

#[test]
fn map_lists_sitemap_urls_first_then_page_links_on_the_same_host() {
    let s = common::start(vec![
        robots("User-agent: *\nSitemap: {origin}/index.xml\n"),
        Route::html("/", PAGE),
        xml("/index.xml", index(&["/s1.xml".into()])),
        xml(
            "/s1.xml",
            urlset(&["/b#frag".into(), "/a".into(), "https://other.test/y".into()]),
        ),
    ]);
    let (code, stdout, stderr) = run(&[&s.url("/"), "--allow-private", "--map"]);
    assert_eq!(code, 0, "stderr={stderr}");
    assert_eq!(
        lines(&stdout),
        vec![s.url("/b"), s.url("/a"), s.url("/nav")],
        "stderr={stderr}"
    );
    assert!(!stderr.contains("warn="), "stderr={stderr}");
}

#[test]
fn map_falls_back_to_sitemap_xml_when_robots_declares_none() {
    let s = common::start(vec![
        ok_robots(),
        Route::html("/", "<html><body>no links</body></html>"),
        typed(
            "/sitemap.xml",
            urlset(&["/only".into()]),
            "text/xml; charset=utf-8",
        ),
    ]);
    let (code, stdout, stderr) = run(&[&s.url("/"), "--allow-private", "--map"]);
    assert_eq!(code, 0, "stderr={stderr}");
    assert_eq!(lines(&stdout), vec![s.url("/only")]);
}

#[test]
fn map_warns_on_missing_sitemap_and_still_lists_page_links() {
    let s = common::start(vec![ok_robots(), Route::html("/", PAGE)]);
    let (code, stdout, stderr) = run(&[&s.url("/"), "--allow-private", "--map"]);
    assert_eq!(code, 0, "stderr={stderr}");
    assert_eq!(lines(&stdout), vec![s.url("/nav"), s.url("/a")]);
    assert!(
        stderr.contains("webgrab: warn=sitemap-failed\n"),
        "stderr={stderr}"
    );
    assert!(stderr.contains("HTTP 404"), "stderr={stderr}");
}

#[test]
fn map_keeps_links_outside_the_extracted_article() {
    let p = "本文として十分な長さのある日本語の段落です。".repeat(8);
    let page = format!(
        r#"<html><head><title>T</title></head><body><nav><a href="/nav">n</a></nav><article><h1>T</h1><p>{p}</p><p>{p}<a href="/in">r</a></p></article></body></html>"#
    );
    let s = common::start(vec![ok_robots(), Route::html("/", page)]);
    let (code, stdout, stderr) = run(&[&s.url("/"), "--allow-private", "--map"]);
    assert_eq!(code, 0, "stderr={stderr}");
    assert_eq!(lines(&stdout), vec![s.url("/nav"), s.url("/in")]);
}

#[test]
fn map_does_not_carry_allow_private_to_a_sitemap_on_another_origin() {
    let other = common::start(vec![
        ok_robots(),
        xml("/internal.xml", urlset(&["/leak".into()])),
    ]);
    let declared = format!("User-agent: *\nSitemap: {}\n", other.url("/internal.xml"));
    let s = common::start(vec![robots(declared), Route::html("/", PAGE)]);
    let (code, stdout, stderr) = run(&[&s.url("/"), "--allow-private", "--map"]);
    assert_eq!(code, 0, "stderr={stderr}");
    assert_eq!(lines(&stdout), vec![s.url("/nav"), s.url("/a")]);
    assert!(
        stderr.contains("webgrab: warn=sitemap-failed\n"),
        "stderr={stderr}"
    );
}

#[test]
fn map_does_not_carry_allow_private_to_an_index_child_on_another_origin() {
    let other = common::start(vec![
        ok_robots(),
        xml("/internal.xml", urlset(&["/leak".into()])),
    ]);
    let s = common::start(vec![
        ok_robots(),
        Route::html("/", PAGE),
        xml("/sitemap.xml", index(&[other.url("/internal.xml")])),
    ]);
    let (code, stdout, stderr) = run(&[&s.url("/"), "--allow-private", "--map"]);
    assert_eq!(code, 0, "stderr={stderr}");
    assert_eq!(lines(&stdout), vec![s.url("/nav"), s.url("/a")]);
    assert!(
        stderr.contains("webgrab: warn=sitemap-failed\n"),
        "stderr={stderr}"
    );
}

#[test]
fn map_ignores_and_counts_relative_sitemap_urls() {
    let s = common::start(vec![
        robots("Sitemap: /declared.xml\n"),
        Route::html("/", "<html><body>x</body></html>"),
        xml("/declared.xml", urlset(&["/from-relative".into()])),
        xml(
            "/sitemap.xml",
            "<urlset><url><loc>{origin}/from-default</loc></url><url><loc>rel</loc></url><url><loc>http://</loc></url></urlset>",
        ),
    ]);
    let (code, stdout, stderr) = run(&[&s.url("/"), "--allow-private", "--map"]);
    assert_eq!(code, 0, "stderr={stderr}");
    assert_eq!(lines(&stdout), vec![s.url("/from-default")]);
    // robots.txtの相対行1件と<loc>の相対値1件を、黙って捨てずに数で知らせる。
    // `http://`は相対ではなく不正な絶対URLなので数えない。
    assert!(
        stderr.contains("webgrab: warn=sitemap-skipped reason=relative-url count=2\n"),
        "stderr={stderr}"
    );
    assert_eq!(
        stderr.matches("reason=relative-url").count(),
        1,
        "stderr={stderr}"
    );
}

#[test]
fn map_with_zero_urls_exits_0_with_empty_body() {
    let s = common::start(vec![
        ok_robots(),
        Route::html("/", "<html><body>none</body></html>"),
        xml("/sitemap.xml", urlset(&[])),
    ]);
    let (code, stdout, stderr) = run(&[&s.url("/"), "--allow-private", "--map"]);
    assert_eq!(code, 0, "stderr={stderr}");
    assert_eq!(stdout.trim(), "");
}

#[test]
fn map_skips_gzip_sitemaps_and_nested_indexes() {
    let s = common::start(vec![
        robots("Sitemap: {origin}/top.xml\nSitemap: {origin}/s.xml.GZ\n"),
        Route::html("/", "<html><body>x</body></html>"),
        xml(
            "/top.xml",
            index(&[
                "/inner.xml".into(),
                "/c.xml.gz".into(),
                "/inner2.xml".into(),
                "/leaf.xml".into(),
            ]),
        ),
        xml(
            "/inner.xml",
            index(&["/deep.xml".into(), "rel-in-nested".into()]),
        ),
        xml("/inner2.xml", index(&["/deep.xml".into()])),
        xml("/deep.xml", urlset(&["/deep-page".into()])),
        xml("/leaf.xml", urlset(&["/leaf-page".into()])),
    ]);
    let (code, stdout, stderr) = run(&[&s.url("/"), "--allow-private", "--map"]);
    assert_eq!(code, 0, "stderr={stderr}");
    assert_eq!(lines(&stdout), vec![s.url("/leaf-page")]);
    assert!(
        stderr.contains("webgrab: warn=sitemap-skipped reason=gzip remaining=2\n"),
        "stderr={stderr}"
    );
    assert_eq!(stderr.matches("reason=gzip").count(), 1, "stderr={stderr}");
    assert!(
        stderr.contains("webgrab: warn=sitemap-skipped reason=nested-index count=2\n"),
        "stderr={stderr}"
    );
    assert_eq!(
        stderr.matches("reason=nested-index").count(),
        1,
        "stderr={stderr}"
    );
    // たどらなかったindexの中身は読まないので、その中の相対URLも数えない。
    assert!(!stderr.contains("relative-url"), "stderr={stderr}");
}

#[test]
fn map_follows_sitemap_redirects_only_within_the_same_host() {
    let other = common::start(vec![
        ok_robots(),
        xml("/moved.xml", urlset(&["/moved-page".into()])),
    ]);
    let away = format!("http://localhost:{}/moved.xml", other.port);
    let s = common::start(vec![
        robots("Sitemap: {origin}/near.xml\nSitemap: {origin}/far.xml\n"),
        Route::html("/", "<html><body>x</body></html>"),
        Route::redirect("/near.xml", "301 Moved Permanently", "/real.xml"),
        xml("/real.xml", urlset(&["/near-page".into()])),
        Route::redirect("/far.xml", "302 Found", away),
    ]);
    let (code, stdout, stderr) = run(&[&s.url("/"), "--allow-private", "--map"]);
    assert_eq!(code, 0, "stderr={stderr}");
    assert_eq!(lines(&stdout), vec![s.url("/near-page")]);
    assert!(
        stderr.contains("redirect to another host refused"),
        "stderr={stderr}"
    );
}

#[test]
fn map_fetches_at_most_twenty_sitemaps() {
    let children: Vec<String> = (0..25).map(|i| format!("/c{i:02}.xml")).collect();
    let mut routes = vec![
        robots("Sitemap: {origin}/index.xml\n"),
        Route::html("/", "<html><body>x</body></html>"),
        xml("/index.xml", index(&children)),
    ];
    for (i, c) in children.iter().enumerate() {
        let path: &'static str = Box::leak(c.clone().into_boxed_str());
        routes.push(xml(path, urlset(&[format!("/p{i:02}")])));
    }
    let s = common::start(routes);
    let (code, stdout, stderr) = run(&[&s.url("/"), "--allow-private", "--map"]);
    assert_eq!(code, 0, "stderr={stderr}");
    let want: Vec<String> = (0..19).map(|i| s.url(&format!("/p{i:02}"))).collect();
    assert_eq!(lines(&stdout), want);
    assert!(
        stderr.contains("webgrab: warn=sitemap-skipped reason=limit remaining=6\n"),
        "stderr={stderr}"
    );
}

#[test]
fn map_cut_short_reports_one_remaining_count_including_children_over_the_limit() {
    let mut children = vec!["/z.xml.gz".to_string()];
    children.extend((0..25).map(|i| format!("/c{i:02}.xml")));
    let big: Vec<String> = (0..200)
        .map(|i| format!("/long-page-name-{i:04}"))
        .collect();
    let s = common::start(vec![
        robots("Sitemap: {origin}/index.xml\n"),
        Route::html("/", "<html><body>x</body></html>"),
        xml("/index.xml", index(&children)),
        xml("/c00.xml", urlset(&big)),
    ]);
    let (code, stdout, stderr) = run(&[
        &s.url("/"),
        "--allow-private",
        "--map",
        "--max-bytes",
        "3000",
    ]);
    assert_eq!(code, 0, "stderr={stderr}");
    assert_eq!(stdout.trim(), "");
    // indexの取得後は19件が待ち、6件が上限外。c00で使い切り、c01の手前で打ち切る。
    assert!(
        stderr.contains("webgrab: warn=sitemap-skipped reason=max-bytes remaining=24\n"),
        "stderr={stderr}"
    );
    assert!(!stderr.contains("reason=limit"), "stderr={stderr}");
    assert!(
        stderr.contains("webgrab: warn=sitemap-skipped reason=gzip remaining=1\n"),
        "stderr={stderr}"
    );
}

#[test]
fn map_obeys_robots_for_sitemaps_unless_no_robots() {
    let routes = || {
        vec![
            robots("User-agent: *\nDisallow: /sitemap.xml\n"),
            Route::html("/", "<html><body>x</body></html>"),
            xml("/sitemap.xml", urlset(&["/p".into()])),
        ]
    };
    let s = common::start(routes());
    let (code, stdout, stderr) = run(&[&s.url("/"), "--allow-private", "--map"]);
    assert_eq!(code, 0, "stderr={stderr}");
    assert_eq!(stdout.trim(), "");
    assert!(stderr.contains("blocked by robots.txt"), "stderr={stderr}");

    let s = common::start(routes());
    let (code, stdout, _) = run(&[&s.url("/"), "--allow-private", "--map", "--no-robots"]);
    assert_eq!(code, 0);
    assert_eq!(lines(&stdout), vec![s.url("/p")]);
}

#[test]
fn map_stops_fetching_sitemaps_when_max_bytes_is_used_up() {
    let big: Vec<String> = (0..200)
        .map(|i| format!("/long-page-name-{i:04}"))
        .collect();
    let s = common::start(vec![
        robots("Sitemap: {origin}/big.xml\nSitemap: {origin}/small.xml\n"),
        Route::html("/", "<html><body>x</body></html>"),
        xml("/big.xml", urlset(&big)),
        xml("/small.xml", urlset(&["/small".into()])),
    ]);
    let (code, stdout, stderr) = run(&[
        &s.url("/"),
        "--allow-private",
        "--map",
        "--max-bytes",
        "2000",
    ]);
    assert_eq!(code, 0, "stderr={stderr}");
    assert_eq!(stdout.trim(), "");
    assert!(stderr.contains("exceeds --max-bytes"), "stderr={stderr}");
    assert!(
        stderr.contains("webgrab: warn=sitemap-skipped reason=max-bytes remaining=1\n"),
        "stderr={stderr}"
    );
}

#[test]
fn map_start_page_failure_keeps_its_exit_code() {
    let s = common::start(vec![ok_robots()]);
    let (code, _, stderr) = run(&[&s.url("/missing"), "--allow-private", "--map"]);
    assert_eq!(code, 4, "stderr={stderr}");
}

#[test]
fn map_conflicts_with_render_flags() {
    for flag in ["--render", "--auto-render", "--raw"] {
        let (code, _, _) = run(&["https://x.test/", "--map", flag]);
        assert_eq!(code, 2, "{flag}");
    }
}

#[test]
fn map_json_and_continuation_keep_the_map_flag() {
    let many: Vec<String> = (0..30).map(|i| format!("/item-{i:02}")).collect();
    let s = common::start(vec![
        ok_robots(),
        Route::html("/", "<html><body>x</body></html>"),
        xml("/sitemap.xml", urlset(&many)),
    ]);
    let (code, stdout, stderr) = run(&[
        &s.url("/"),
        "--allow-private",
        "--map",
        "--format",
        "json",
        "--max-chars",
        "100",
    ]);
    assert_eq!(code, 0, "stderr={stderr}");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect("valid json");
    assert_eq!(v["truncated"], true);
    let next = v["continue_command"].to_string();
    assert!(next.contains("--map"), "{v}");
}
