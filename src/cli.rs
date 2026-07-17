//! CLI定義（設計§5、clap derive）。

use clap::{Parser, ValueEnum};

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum FormatArg {
    Markdown,
    Frontmatter,
    Json,
    Text,
    Html,
}

/// webgrab — LLM向けWeb情報取得ツール。ページ本文を切り捨てずにMarkdownで返す。
#[derive(Debug, Parser)]
#[command(name = "webgrab", version, about, long_about = None)]
#[command(after_help = EXIT_CODE_HELP)]
pub struct Cli {
    /// 取得するURL（http/httpsのみ）
    pub url: String,

    /// 出力形式
    #[arg(long, value_enum, default_value_t = FormatArg::Markdown)]
    pub format: FormatArg,

    /// 本文の最大文字数（Unicodeスカラー値）。0でメタのみ
    #[arg(long, default_value_t = 24000)]
    pub max_chars: usize,

    /// 本文の開始文字オフセット（続き取得用）
    #[arg(long, default_value_t = 0)]
    pub start_index: usize,

    /// JSレンダリング（Chrome）を使う
    #[arg(long, default_value_t = false)]
    pub render: bool,

    /// --render時、ロード後の追加待機ミリ秒
    #[arg(long, default_value_t = 2000)]
    pub wait_ms: u64,

    /// 本文抽出をスキップしページ全体を変換
    #[arg(long, default_value_t = false)]
    pub raw: bool,

    /// 全体タイムアウト（秒）
    #[arg(long, default_value_t = 30)]
    pub timeout: u64,

    /// robots.txt確認をスキップ
    #[arg(long, default_value_t = false)]
    pub no_robots: bool,

    /// 内部アドレスの取得を許可（SSRF保護を解除）
    #[arg(long, default_value_t = false)]
    pub allow_private: bool,

    /// トークン計測を省略
    #[arg(long, default_value_t = false)]
    pub no_tokens: bool,

    /// 本文を非信頼コンテンツフェンスで囲む（プロンプトインジェクション緩和）
    #[arg(long, default_value_t = false)]
    pub fence: bool,

    /// User-Agentの上書き
    #[arg(long)]
    pub user_agent: Option<String>,

    /// 取得データの上限バイト数（展開後）
    #[arg(long, default_value_t = 20 * 1024 * 1024)]
    pub max_bytes: u64,

    /// 出力ファイル（省略時はstdout）
    #[arg(short, long)]
    pub output: Option<String>,

    /// Chrome実行ファイルのパス（--render時、自動検出に失敗する場合）
    #[arg(long)]
    pub chrome_path: Option<String>,
}

const EXIT_CODE_HELP: &str = "\
EXIT CODES:
  0  success
  1  internal error (incl. output file write failure)
  2  usage / invalid URL
  3  network failure (DNS/connect/timeout/TLS/redirect loop; retryable)
  4  HTTP error (4xx/5xx), size over, non-HTML
  5  blocked by robots.txt
  6  empty body (0 chars extracted)
  7  JS render failure (Chrome missing/launch/CDP/timeout)
  8  internal address refused (use --allow-private)";

/// デフォルトUA（設計§5）。
pub fn default_user_agent() -> String {
    format!(
        "webgrab/{} (+https://github.com/okamyuji/webgrab)",
        env!("CARGO_PKG_VERSION")
    )
}

/// 継続コマンド再現用に、非デフォルトフラグを再構成する（--start-indexと-oは除外）。
pub fn extra_flags(cli: &Cli) -> Vec<String> {
    let mut v = Vec::new();
    if cli.format != FormatArg::Markdown {
        let f = match cli.format {
            FormatArg::Markdown => "markdown",
            FormatArg::Frontmatter => "frontmatter",
            FormatArg::Json => "json",
            FormatArg::Text => "text",
            FormatArg::Html => "html",
        };
        v.push(format!("--format {f}"));
    }
    if cli.max_chars != 24000 {
        v.push(format!("--max-chars {}", cli.max_chars));
    }
    if cli.render {
        v.push("--render".into());
    }
    if cli.raw {
        v.push("--raw".into());
    }
    if cli.no_tokens {
        v.push("--no-tokens".into());
    }
    if cli.fence {
        v.push("--fence".into());
    }
    if cli.allow_private {
        v.push("--allow-private".into());
    }
    // 続き取得で取得結果が変わりうる残りの非デフォルトフラグも再現する（設計§6）
    if cli.wait_ms != 2000 {
        v.push(format!("--wait-ms {}", cli.wait_ms));
    }
    if cli.timeout != 30 {
        v.push(format!("--timeout {}", cli.timeout));
    }
    if cli.no_robots {
        v.push("--no-robots".into());
    }
    if cli.max_bytes != 20 * 1024 * 1024 {
        v.push(format!("--max-bytes {}", cli.max_bytes));
    }
    if let Some(ua) = &cli.user_agent {
        v.push(format!("--user-agent '{ua}'"));
    }
    if let Some(cp) = &cli.chrome_path {
        v.push(format!("--chrome-path '{cp}'"));
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal() {
        let cli = Cli::try_parse_from(["webgrab", "https://x.test"]).unwrap();
        assert_eq!(cli.url, "https://x.test");
        assert_eq!(cli.max_chars, 24000);
        assert_eq!(cli.format, FormatArg::Markdown);
    }

    #[test]
    fn extra_flags_excludes_start_index_and_output() {
        let cli = Cli::try_parse_from([
            "webgrab",
            "https://x.test",
            "--render",
            "--format",
            "json",
            "--start-index",
            "5000",
            "-o",
            "out.md",
        ])
        .unwrap();
        let f = extra_flags(&cli);
        assert!(f.contains(&"--render".to_string()));
        assert!(f.contains(&"--format json".to_string()));
        assert!(!f.iter().any(|s| s.contains("start-index")));
        assert!(!f.iter().any(|s| s.contains("-o") || s.contains("output")));
    }

    #[test]
    fn default_ua_has_product_token() {
        assert!(default_user_agent().starts_with("webgrab/"));
    }
}
