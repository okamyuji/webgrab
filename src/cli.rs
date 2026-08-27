//! CLI定義（設計§5、clap derive）。

use clap::{Parser, ValueEnum};

pub const DEFAULT_WAIT_MS: u64 = 5000;

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

    /// 自動レンダリング（static取得失敗時のみChrome使用）
    #[arg(long, default_value_t = false)]
    pub auto_render: bool,

    /// --render / --auto-render時、goto開始からDOM取得までの上限ミリ秒（既定5000）
    #[arg(long)]
    pub wait_ms: Option<u64>,

    /// Chromeサンドボックスを無効化（セキュリティと安定性のトレードオフ）
    #[arg(long, default_value_t = false)]
    pub no_sandbox: bool,

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
  6  empty body (0 chars extracted; see hint= on stderr)
  7  JS render failure (Chrome missing/launch/CDP/timeout)
  8  internal address refused (use --allow-private)";

/// デフォルトUA（設計§5）。
pub fn default_user_agent() -> String {
    format!(
        "webgrab/{} (+https://github.com/okamyuji/webgrab)",
        env!("CARGO_PKG_VERSION")
    )
}

/// 継続コマンド再現用に、非デフォルトフラグを再構成する（--start-indexと-oは除外、設計§4.3 6）。
/// - `status`が`Rendered`なら`--auto-render`を`--render`に置換し、render系フラグも再現する
/// - それ以外なら`--auto-render`とrender系（--wait-ms/--no-sandbox/--chrome-path）を省略する
pub fn extra_flags(cli: &Cli, status: crate::output::RenderStatus) -> Vec<String> {
    use crate::budget::shell_quote;
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
    let render_path = cli.render || (cli.auto_render && status.is_rendered());
    if render_path {
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
    if render_path {
        if let Some(w) = cli.wait_ms {
            v.push(format!("--wait-ms {w}"));
        }
        if cli.no_sandbox {
            v.push("--no-sandbox".into());
        }
        if let Some(cp) = &cli.chrome_path {
            v.push(format!("--chrome-path {}", shell_quote(cp)));
        }
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
        v.push(format!("--user-agent {}", shell_quote(ua)));
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::RenderStatus;

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
        let f = extra_flags(&cli, RenderStatus::Rendered);
        assert!(f.contains(&"--render".to_string()));
        assert!(f.contains(&"--format json".to_string()));
        assert!(!f.iter().any(|s| s.contains("start-index")));
        assert!(!f.iter().any(|s| s.contains("-o") || s.contains("output")));
    }

    #[test]
    fn auto_render_is_replaced_by_render_only_when_rendered() {
        let cli = Cli::try_parse_from([
            "webgrab",
            "https://x.test",
            "--auto-render",
            "--no-sandbox",
            "--wait-ms",
            "3000",
        ])
        .unwrap();
        let r = extra_flags(&cli, RenderStatus::Rendered);
        assert_eq!(r.iter().filter(|s| *s == "--render").count(), 1);
        assert!(!r.iter().any(|s| s == "--auto-render"));
        assert!(r.contains(&"--no-sandbox".to_string()));
        assert!(r.contains(&"--wait-ms 3000".to_string()));
        for st in [
            RenderStatus::Static,
            RenderStatus::NoGain,
            RenderStatus::Failed("render"),
            RenderStatus::Skipped("timeout"),
        ] {
            let f = extra_flags(&cli, st);
            assert!(!f.iter().any(|s| s.contains("render")), "{st:?}: {f:?}");
            assert!(
                !f.iter()
                    .any(|s| s.contains("sandbox") || s.contains("wait-ms")),
                "{st:?}: {f:?}"
            );
        }
    }

    #[test]
    fn explicit_render_with_auto_render_emits_render_once() {
        let cli = Cli::try_parse_from(["webgrab", "https://x.test", "--render", "--auto-render"])
            .unwrap();
        let f = extra_flags(&cli, RenderStatus::Rendered);
        assert_eq!(f.iter().filter(|s| *s == "--render").count(), 1);
    }

    #[test]
    fn wait_ms_default_is_not_reproduced_but_explicit_is() {
        let d = Cli::try_parse_from(["webgrab", "https://x.test", "--render"]).unwrap();
        assert_eq!(d.wait_ms, None);
        assert!(
            !extra_flags(&d, RenderStatus::Rendered)
                .iter()
                .any(|s| s.contains("wait-ms"))
        );
        let e = Cli::try_parse_from(["webgrab", "https://x.test", "--render", "--wait-ms", "2000"])
            .unwrap();
        assert!(extra_flags(&e, RenderStatus::Rendered).contains(&"--wait-ms 2000".to_string()));
        let same =
            Cli::try_parse_from(["webgrab", "https://x.test", "--render", "--wait-ms", "5000"])
                .unwrap();
        assert!(extra_flags(&same, RenderStatus::Rendered).contains(&"--wait-ms 5000".to_string()));
    }

    #[test]
    fn value_flags_are_shell_quoted() {
        let cli = Cli::try_parse_from([
            "webgrab",
            "https://x.test",
            "--render",
            "--user-agent",
            "a'; id; #",
            "--chrome-path",
            "/opt/x y",
        ])
        .unwrap();
        let f = extra_flags(&cli, RenderStatus::Rendered);
        assert!(
            f.contains(&r"--user-agent 'a'\''; id; #'".to_string()),
            "{f:?}"
        );
        assert!(f.contains(&"--chrome-path '/opt/x y'".to_string()), "{f:?}");
    }

    #[test]
    fn default_ua_has_product_token() {
        assert!(default_user_agent().starts_with("webgrab/"));
    }
}
