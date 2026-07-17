//! 設計書§8のsmoke test (1)(2)(3): chromiumoxide+Chrome CDP接続、htmd表変換、dom_smoothie公開日時API
use chromiumoxide::browser::{Browser, BrowserConfig};
use futures::StreamExt;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // (2) htmd table conversion
    let table_html = "<table><tr><th>A</th><th>B</th></tr><tr><td>1</td><td>2</td></tr></table>";
    let md = htmd::convert(table_html)?;
    println!("SMOKE2 htmd table output: {md:?}");

    // (3) dom_smoothie metadata fields
    let doc = r#"<html><head><title>T</title>
      <meta property="article:published_time" content="2026-01-02T03:04:05Z">
      </head><body><article><h1>T</h1><p>hello world content for extraction test. more words here to pass thresholds. and more and more.</p></article></body></html>"#;
    let cfg = dom_smoothie::Config::default();
    let mut readability = dom_smoothie::Readability::new(doc, None, Some(cfg))?;
    let article = readability.parse()?;
    println!(
        "SMOKE3 dom_smoothie: title={:?} published_time={:?}",
        article.title, article.published_time
    );

    // (1) chromiumoxide + real Chrome
    let chrome = "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome";
    let (mut browser, mut handler) = Browser::launch(
        BrowserConfig::builder()
            .chrome_executable(chrome)
            .new_headless_mode()
            .build()?,
    )
    .await?;
    let handle = tokio::spawn(async move { while handler.next().await.is_some() {} });
    let page = browser.new_page("https://example.com").await?;
    page.wait_for_navigation().await?;
    let content = page.content().await?;
    println!(
        "SMOKE1 chromiumoxide: fetched {} bytes, contains 'Example Domain' = {}",
        content.len(),
        content.contains("Example Domain")
    );
    browser.close().await?;
    let _ = handle.await;
    Ok(())
}
