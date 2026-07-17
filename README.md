# webgrab

LLM（Claude Code / Codex CLI / Kimi CLI など）向けのWeb情報取得CLIです。Webページの本文を切り捨てずに、制御可能な量で、LLMが読みやすいMarkdownとして標準出力に返します。

## なぜ作ったか

Claude Code環境で計測したところ、標準のWeb取得には次の制約がありました（詳細は[docs/01-measurement-report.md](docs/01-measurement-report.md)）。

- WebFetchは取得内容を**ちょうど100,000文字で切断**し、さらに小型モデルの要約だけをメインモデルに渡す
- WebSearchはリンクと約1,900文字の生成要約しか返さず、ページ原文は渡らない

つまりLLMはページの原文全体を見られていません。webgrabはこの欠損を埋めます。

## curlとの違い（差別化）

| 観点 | curl | webgrab |
|---|---|---|
| 本文抽出 | しない（nav/footer/広告も全部） | dom_smoothie（Readability）でボイラープレート除去 |
| 出力形式 | 生HTML | LLM向けMarkdown（Jina Reader互換ヘッダ付き） |
| JSレンダリング | 不可 | `--render`でChrome経由（SPA対応） |
| 量の制御 | なし | `--max-chars`/`--start-index`でページング、トークン概算を表示 |
| 文字コード | 手動 | ヘッダ→meta→推定の3段自動判定（Shift_JIS等の日本語ページ対応） |
| SSRF防止 | なし | 内部アドレス（メタデータエンドポイント等）をデフォルト拒否 |
| エージェント連携 | 終了コードのみ | 機械可読なstderr書式・続き取得コマンドの自己提示 |

「原文をそのままLLMに渡す」のではなく、「LLMが読むべき本文だけを、文脈に収まる量で、続きも取れる形で渡す」のがwebgrabです。

## インストール

```bash
cargo install --path .
# または
cargo build --release   # target/release/webgrab
```

`--render`を使う場合はGoogle Chrome / Chromiumが必要です。

## 使い方

```bash
webgrab https://example.com/article
webgrab https://spa.example.com --render          # JSレンダリング
webgrab https://example.com --format json         # プログラム連携用
webgrab https://example.com --max-chars 8000      # 量を絞る
webgrab https://example.com --start-index 8000    # 続きを取る
```

出力（デフォルトのmarkdown形式）:

```text
Title: 記事タイトル
URL Source: https://example.com/article
Published Time: 2026-01-02T03:04:05Z
Tokens: 1234 (chars: 5000 / total: 12000)

Markdown Content:
（本文Markdown）
[webgrab:truncated chars 0-5000 of 12000 — continue: webgrab https://example.com/article --start-index 5000]
```

本文はstdout、警告・エラーはstderr、失敗時は非0終了コード（`--help`に一覧）です。

## 終了コード

| コード | 意味 |
|---|---|
| 0 | 成功 |
| 2 | 引数・URL形式エラー |
| 3 | ネットワーク失敗（リトライ可） |
| 4 | HTTPエラー・サイズ超過・非HTML |
| 5 | robots.txtによる拒否 |
| 6 | 本文が空 |
| 7 | JSレンダリング失敗 |
| 8 | 内部アドレス拒否（`--allow-private`で解除） |

## 各LLMからの呼び出しサンプル

[samples/skills/](samples/skills/) に、Claude Code / Codex CLI / Kimi CLI 向けのサンプルSKILL・設定を同梱しています。これらは**サンプル**であり、このリポジトリ自体の開発に適用されるルールとは別物です。

## 設計と品質

設計書・調査・レビュー記録は [docs/](docs/) にあります。設計は敵対的レビュー3ラウンドを経てv1.2でsealされています。テストカバレッジは約89%です。
