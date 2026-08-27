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
webgrab https://spa.example.com --auto-render     # 本文が空か200文字未満のときだけJSレンダリングへ自動切替
webgrab https://example.com --format json         # プログラム連携用
webgrab https://example.com --max-chars 8000      # 量を絞る
webgrab https://example.com --start-index 8000    # 続きを取る
webgrab https://example.com --format json --fence # エージェント用途の推奨（本文分離＋境界明示）
```

`--auto-render`は既定off。静的取得の本文が空または200文字未満のとき、同一プロセス内でJSレンダリングへ切り替えます。切替・失敗・skipはstderrと`--format json`の`render_status`（`static`/`rendered`/`failed`/`no-gain`/`skipped`）で分かります。`--wait-ms`（既定5000、`--render`/`--auto-render`時のみ有効）はrender取得の上限ミリ秒です。ネットワーク静止・DOM安定・可視テキスト200文字以上を満たせば上限前に終了します。

出力（デフォルトのmarkdown形式）:

```text
Title: 記事タイトル
URL Source: https://example.com/article
Published Time: 2026-01-02T03:04:05Z
Tokens: 1234 (chars: 5000 / total: 12000)

Markdown Content:
（本文Markdown）
[webgrab:truncated chars 0-5000 of 12000 — continue: webgrab 'https://example.com/article' --start-index 5000]
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

## セキュリティと信頼モデル

webgrabは任意のWebページ本文をLLMへ渡すため、取得内容は**信頼できない外部データ**として扱う必要があります。ツール側では次の緩和を行いますが、これは攻撃面の縮小であって完全防御ではありません。

- SSRF防止（内部アドレス拒否・IPピン留め・render経路の検証プロキシ）。詳細は[設計書](docs/04-design.md)§3.1。
- 出力インジェクション対策: 端末制御文字（ANSI/OSC）除去、`javascript:`等の危険リンクスキーム無害化、非信頼タイトルによるヘッダ/YAML偽造の防止、本文からのwebgrab制御マーカー偽造の防止。
- `--fence`: 本文を `[webgrab:untrusted-content ...]` 〜 `[webgrab:untrusted-content-end]` で囲み、外部データの境界を明示する（本文からは閉じマーカーを偽造できない）。`--format json` では本文が `markdown` フィールドに構造的に分離され、`untrusted: true` と `untrusted_note` で同じ非信頼シグナルを渡す。
- `--no-sandbox`: Chromeのsandboxはページ側HTML/JSに対する封じ込め層です。無効化するとその層が失われるため、sandboxが起動しないCI環境等に限って指定してください。通常利用では指定しないでください。指定時はstderrに`warn=no-sandbox`が出て、続き取得の継続コマンドにも再現されます。
- `--auto-render`: 外部ページは本文を短く返すだけでChrome起動を誘発できます。エスカレーション時は対象オリジンへ描画1ページ分（サブリソース含む）の追加要求が発生するため、自側のコストは残余`--timeout`と残余`--max-bytes`で上限づけられます。一覧ページや連続取得を大量に行う用途では既定にしないでください。

**プロンプトインジェクションの根本防御は消費側の責務です。** 散文中の説得型指示（「以前の指示を無視せよ」等）はツールでは防げません。呼び出す側のエージェントは、取得本文を指示として自動実行せず、破壊的・外部影響のある操作は人間確認を挟んでください。エージェント用途では、本文が構造的に分離される `--format json` の利用と `--fence` の併用を推奨します。

### 無害化の検証スクリプト

上記の無害化が実際に効いていることは、実バイナリで再現検証できます。

```bash
scripts/security-verify/run.sh
```

[scripts/security-verify/](scripts/security-verify/) は、端末エスケープ・危険リンク・webgrab制御マーカー偽造・タイトル改行によるヘッダ偽造・説得型テキストを1ページに詰めた**無害な**インジェクションページ（`injection_page.py`）をローカルに立て、webgrabを実行して各ベクトルの無害化を PASS/FAIL で判定します。SSRF（内部アドレス拒否=終了コード8、不正スキーム拒否=終了コード2）も併せて確認します。`python3` と `curl` があれば動作し、Chromeは不要です（release binaryが無ければ自動ビルド）。1つでも失敗すると非0終了します。

## 各LLMからの呼び出しサンプル

[samples/skills/](samples/skills/) に、Claude Code / Codex CLI / Kimi CLI 向けのサンプルSKILL・設定を同梱しています。これらは**サンプル**であり、このリポジトリ自体の開発に適用されるルールとは別物です。

## 設計と品質

設計書・調査・レビュー記録は [docs/](docs/) にあります。設計は敵対的レビュー3ラウンドを経てv1.2でsealされ、その後の実装敵対的レビューでSSRF・出力インジェクション・DoSの修正とOWASP対応（A10/A03）を追加しています（記録は [docs/_quality/IMPROVEMENT_BACKLOG.md](docs/_quality/IMPROVEMENT_BACKLOG.md)）。ユニット・統合あわせて111件のテストが通ります（`cargo test`）。
