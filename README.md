# webgrab

LLM（Claude Code、Codex CLI、Kimi CLIなど）向けのWeb情報取得CLIです。Webページの本文を切り捨てずに、制御可能な量で、LLMが読みやすいMarkdownとして標準出力に返します。

## なぜ作ったか

Claude Code環境で計測したところ、標準のWeb取得には次の制約がありました（詳細は[docs/01-measurement-report.md](docs/01-measurement-report.md)）。

- WebFetchは取得内容をちょうど100,000文字で切断し、さらに小型モデルの要約だけをメインモデルに渡す
- WebSearchはリンクと約1,900文字の生成要約しか返さず、ページ原文は渡らない

つまりLLMはページの原文全体を見られていません。webgrabはこの欠損を埋めます。

## curlとの違い（差別化）

| 観点 | curl | webgrab |
|---|---|---|
| 本文抽出 | しない（nav/footer/広告も全部） | dom_smoothie（Readability）でボイラープレート除去 |
| 出力形式 | 生HTML | LLM向けMarkdown（Jina Reader互換ヘッダ付き） |
| JSレンダリング | 不可 | `--render`でChrome経由（SPA対応） |
| 量の制御 | なし | `--max-chars`/`--start-index`でページング、改行境界で切り詰め、トークン概算を表示 |
| 文字コード | 手動 | ヘッダ→meta→推定の3段自動判定（Shift_JIS等の日本語ページ対応） |
| プレーンテキスト | そのまま | `text/plain`（ソースコード、`llms.txt`等）は抽出を通さず素通し（改行と`<T>`を保持） |
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
webgrab https://example.com --format links        # ページ内リンクを絶対URLで1行ずつ（--rawでページ全体）
webgrab https://example.com --map                 # 同じホストのURL一覧（sitemapと起点ページのリンクから）
webgrab https://example.com --max-chars 8000      # 量を絞る
webgrab https://example.com --start-index 8000    # 続きを取る
webgrab https://example.com --format json --fence # エージェント用途の推奨（本文分離＋境界明示）
webgrab https://raw.githubusercontent.com/<owner>/<repo>/main/README.md   # text/plainはそのまま返す
```

`--auto-render`は既定off。静的取得の本文が空または200文字未満のとき、同一プロセス内でJSレンダリングへ切り替えます。切替・失敗・skipはstderrと`--format json`の`render_status`（`static`/`rendered`/`failed`/`no-gain`/`skipped`）で分かります。`--wait-ms`（既定5000、`--render`/`--auto-render`時のみ有効）はrender取得の上限ミリ秒です。ネットワーク静止・DOM安定・可視テキスト200文字以上を満たせば上限前に終了します。

`--map`は本文の代わりに、起点と同じホストのURLを1行1件で返します。並び順はsitemapのURLが先、起点ページのリンクが後で、重複とfragmentは除かれます。sitemapの入口はrobots.txtの`Sitemap:`行で、行が無いときは`/sitemap.xml`です。sitemap indexは1段だけたどり、gzipのsitemapは読みません。`Sitemap:`行と`<loc>`は完全なURLだけを読みます（sitemaps.orgの規約）。読まなかった相対URLの件数は`warn=sitemap-skipped reason=relative-url`に出ます。sitemapの取得は20件が上限で、本文の取得は残りの`--timeout`と`--max-bytes`の範囲に収めます。打ち切りや取得失敗はstderrの`warn=sitemap-skipped`と`warn=sitemap-failed`に出ますが、その場合も起点ページのリンクで結果を返します。`warn=sitemap-skipped`は理由ごとに1行で、取得しなかったsitemapの数を`remaining=`、取得したsitemapの中で読み飛ばした数（`relative-url`と`nested-index`）を`count=`で示します。URL一覧にも本文と同じ`--max-chars`の上限がかかり、続きは`--start-index`で取ります。続きを取るたびにsitemapを最初から取り直すので、大きなサイトの全件を一度に取るときは`--max-chars`を大きくしてください（`0`はメタ情報だけを返します）。`--render`・`--auto-render`・`--raw`・`--wait-ms`とは併用できません。

デフォルトのmarkdown形式の出力を次に示します。

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
| 3 | ネットワーク失敗・取得が`--timeout`を超過（リトライ可） |
| 4 | HTTPエラー・サイズ超過・未対応のContent-Type・取得後の処理が`--timeout`を超過（403は`hint=--render`付き。`--auto-render`の描画後の処理が超過した場合は静的取得の結果を返す） |
| 5 | robots.txtによる拒否 |
| 6 | 本文が空 |
| 7 | JSレンダリング失敗 |
| 8 | 内部アドレス拒否（`--allow-private`で解除） |

`text/plain`の本文は抽出を通さないため、本文が空でも終了コード0です。

## セキュリティと信頼モデル

webgrabは任意のWebページ本文をLLMへ渡すため、取得内容は信頼できない外部データとして扱う必要があります。ツール側では次の緩和を行いますが、これは攻撃面の縮小であって完全防御ではありません。

- SSRF防止（内部アドレス拒否・IPピン留め・render経路の検証プロキシ）。詳細は[設計書](docs/04-design.md)§3.1。`--render`使用時の`URL Source`はrender後URL（`location.href`、userinfoを除去）です。
- 出力インジェクション対策: 端末制御文字（ANSI/OSC）除去、`javascript:`等の危険リンクスキーム無害化、非信頼タイトルによるヘッダ/YAML偽造の防止、本文からのwebgrab制御マーカー偽造の防止。`text/plain`の本文では、危険リンクスキームの無害化はMarkdownのインラインリンク・オートリンク・参照定義の3形だけが対象です。生のHTMLタグは無害化しません（`Mutex<T>`のようなソースコードを壊さないためです）。出力をHTMLとして描画する消費側は、自身でHTMLタグの無害化を行ってください。
- `--fence`: 本文を `[webgrab:untrusted-content ...]` 〜 `[webgrab:untrusted-content-end]` で囲み、外部データの境界を明示する（本文からは閉じマーカーを偽造できない）。`--format json` では本文が `markdown` フィールドに構造的に分離され、`untrusted: true` と `untrusted_note` で同じ非信頼シグナルを渡す。
- `--no-sandbox`: Chromeのsandboxはページ側HTML/JSに対する封じ込め層です。無効化するとその層が失われるため、sandboxが起動しないCI環境等に限って指定してください。通常利用では指定しないでください。指定時はstderrに`warn=no-sandbox`が出て、続き取得の継続コマンドにも再現されます。
- `--auto-render`: 外部ページは本文を短く返すだけでChrome起動を誘発できます。エスカレーション時は対象オリジンへ描画1ページ分（サブリソース含む）の追加要求が発生するため、自側のコストは残余`--timeout`と残余`--max-bytes`で上限づけられます。一覧ページや連続取得を大量に行う用途では既定にしないでください。
- `--map`: 起点ページに加えてsitemapを最大20件取得します。robots.txtもsitemapの入口を探すために1件取得します。`--no-robots`が無ければ、sitemapごとの判定のためにも取得します。判定のための取得は固定の上限（5秒、512KiB）で行い、`--max-bytes`の残りには数えません。時間は、その判定を含む各取得の`--timeout`の中に収めます。各取得は通常の取得と同じ内部アドレス拒否を通ります。`--allow-private`を引き継ぐのは、起点と同じオリジンのURLを指すsitemapの取得だけです。robots.txtやsitemap indexが別オリジンの内部アドレスを指しても、webgrabはそこへ要求を出しません。ただし同じオリジンのsitemapが転送した先へは、起点ページの転送と同じく`--allow-private`のまま要求します。sitemapの転送と、sitemapごとのrobots.txtの判定の転送は、今のホストか起点のホストへだけたどります。判定のためのrobots.txtが第三者のホストへ転送された場合は、取得できなかったときと同じく許可として扱います。悪意のあるsitemapが転送を重ねて、第三者へ多くの要求を送らせることを防ぐためです。ただし、robots.txtの`Sitemap:`行やsitemap indexが別のホストを直接指す場合は、そのホストへsitemapとrobots.txtの判定の要求を出します（sitemapは最大20件）。

プロンプトインジェクションの根本防御は消費側の責務です。本文中の説得型指示（「以前の指示を無視せよ」等）はツールでは防げません。呼び出す側のエージェントは、取得本文を指示として自動実行せず、破壊的・外部影響のある操作は人間確認を挟んでください。エージェント用途では、本文が構造的に分離される `--format json` の利用と `--fence` の併用を推奨します。

### 無害化の検証スクリプト

上記の無害化が実際に機能していることは、実バイナリで再現検証できます。

```bash
scripts/security-verify/run.sh
```

[scripts/security-verify/](scripts/security-verify/)は、端末エスケープ・危険リンク・webgrab制御マーカー偽造・タイトル改行によるヘッダ偽造・説得型テキストを1ページに詰めた無害なインジェクションページ（`injection_page.py`）をローカルに立て、webgrabを実行して各ベクトルの無害化をPASS/FAILで判定します。SSRF（内部アドレス拒否=終了コード8、不正スキーム拒否=終了コード2）も併せて確認します。`python3` と `curl` があれば動作し、Chromeは不要です（release binaryが無ければ自動ビルド）。1つでも失敗すると非0終了します。

## 各LLMからの呼び出しサンプル

[samples/skills/](samples/skills/)に、Claude Code、Codex CLI、Kimi CLI向けのサンプルSKILL・設定を同梱しています。これらはサンプルであり、このリポジトリ自体の開発に適用されるルールとは別物です。

## 設計と品質

設計書と調査記録は [docs/](docs/) にあります。テストは `cargo test` で実行します。

## ライセンス

[MIT](LICENSE)
