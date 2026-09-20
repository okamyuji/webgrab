# 実動作検証報告書 — webgrab

- バージョン: 1.0
- 日付: 2026-07-17
- 対応計画: [06-verification-plan.md](06-verification-plan.md)
- 検証バイナリ: target/release/webgrab（cargo 1.97.1 / edition 2024）

## A. 静的検証

| 項目 | コマンド | 観測 | 判定 |
|---|---|---|---|
| A1 build | `cargo build --release` | `Finished release profile` exit 0 | 合格 |
| A2 test | `cargo test` | unit 57 + integration 9 = 66件すべてpass、exit 0（実装レビュー修正後） | 合格 |
| A3 clippy | `cargo clippy --all-targets -- -D warnings` | exit 0（警告0） | 合格 |
| A4 coverage | `cargo llvm-cov --ignore-filename-regex 'render\.rs\|main\.rs'` | line coverage **88.08%**（TOTAL 1124行中134 missed） | 合格（60%超・80%目標達成） |

主要モジュールのカバレッジ: netguard 100%、budget 100%、tokens 100%、output 98.91%、robots 97.44%、decode 98.61%。SSRF核心のnetguardは全行カバー。

## B. 実URL取得

### B1. 大型ページ（RFC 9110全文、455,255文字）

- Ran: `webgrab https://www.rfc-editor.org/rfc/rfc9110.txt --max-chars 8000`
- Observed: `Tokens: 2617 (chars: 8000 / total: 455255)`。本文先頭からMarkdown出力。末尾に `[webgrab:truncated chars 0-8000 of 455255 — continue: ... --start-index 8000]`。exit 0
- 判定: 合格。WebFetchが100,000文字で切断するのに対し、webgrabは455,255文字全体を認識し、指定量で切り出し、続きの取得コマンドを自己提示した（Zenn記事の主張する制約を解消できることの実証）

### B2. 日本語ページ（Wikipedia「Rust (プログラミング言語)」、152,237文字）

- Ran: `webgrab "https://ja.wikipedia.org/wiki/Rust_(プログラミング言語)" --max-chars 1200`
- Observed: `Title: Rust (プログラミング言語)` / `Published Time: 2012-07-18T13:56:41Z` / `Tokens: 184 (chars: 1200 / total: 152237)`。本文の日本語・リンク・表がMarkdownに変換され文字化けなし。exit 0
- 判定: 合格。文字コード判定・title/公開日時抽出・マルチバイト境界スライスが正常動作

### B3. SPA/JSレンダリング（--render、Chrome CDP Fetch interception経路）

- Ran: `webgrab https://example.com --render --max-chars 500`
- Observed: `Title: Example Domain` / 本文Markdown出力 / exit 0。Chromeが起動しrequest interception（netguard判定）を通してDOMを取得、プロセスは正常終了（ゾンビ・一時プロファイル残留なし）
- 判定: 合格。render経路が設計§3.1のinterception付きで端から端まで動作

## C. 続き取得・形式

### C1. 続き取得（--start-index）

- Ran: `webgrab "https://ja.wikipedia.org/wiki/Rust_(プログラミング言語)" --max-chars 400 --start-index 1200`
- Observed: `Tokens: 6 (chars: 400 / total: 152237)`、フッタが `chars 1200-1600 of 152237` と正しいオフセットに前進。exit 0
- 判定: 合格

### C2. JSON形式

- 統合テスト `json_format_emits_valid_json` でserde_jsonがパース可能なエンベロープ（markdown+metadata、truncated=false）を確認。合格

## D. エラー・安全経路（統合テスト、tests/integration.rs）

| 項目 | テスト | 観測 | 判定 |
|---|---|---|---|
| D1 HTTP 404 | `http_404_returns_exit_4` | exit 4、stderr `error=http` | 合格 |
| D2 robots拒否 | `robots_disallow_returns_exit_5` | exit 5、stderr `error=robots` | 合格 |
| D3 内部アドレス | `internal_address_without_flag_returns_exit_8` | exit 8、stderr `error=netguard`（SSRF防止発火） | 合格 |
| D4 不正スキーム | `invalid_scheme_returns_exit_2` | exit 2、stderr `error=usage` | 合格 |
| — stdout/stderr分離 | `stdout_stderr_separation_on_success` | 本文はstdoutのみ、stderrに本文混入なし | 合格 |

## 実装中に検出・修正したバグ（テスト・レビューの効果）

- robots.txt取得URLがポート番号を欠落しており、非標準ポートのサーバでrobotsが常に「許可」扱いになる不具合を統合テスト`robots_disallow_returns_exit_5`が検出。`fetch.rs`のauthority構築を修正して解消。
- 実装レビュー（rust-reviewer）がCRITICAL C1（robots.txt取得がreqwest自動リダイレクトでnetguard未検証＝SSRFバイパス）を検出。手動1回追従+追従先再検証に修正し、回帰テスト `robots_redirect_is_manually_followed_once` を追加して緑を確認。HIGH 3件（async内同期DNS、継続コマンドのフラグ取りこぼし、renderの一時プロファイル未削除）も修正済み。詳細は [_quality/IMPROVEMENT_BACKLOG.md](_quality/IMPROVEMENT_BACKLOG.md)。

## 実装レビュー修正後の再検証（2026-07-17）

- Ran: `webgrab https://example.com --render --max-chars 300` → Exit 0 → Observed: 本文取得成功。一時プロファイルディレクトリ `webgrab-chrome-*` の残留を確認したところ0件（H3のTempDir RAII削除が実機で機能）。
- Ran: `cargo test` → Exit 0 → Observed: 66件pass（C1回帰テスト含む）。

## 総合判定

設計§9の完了条件1〜5をすべて観測エビデンス付きで満たした。webgrabは、Zenn記事が指摘した「LLMのWeb取得は先頭しか読まない／要約しか得られない」という制約を、実URLで実際に解消できることを実証した。

## JS描画改善の検証（2026-08-27）

設計08 §8の手順で、リリースビルドと実在のJS描画ページ3件を対象に検証した。独立経路は、同じChromeを`--headless=new --dump-dom`で直接起動して取得したDOMを使った。Claude Code内蔵のブラウザツールは使っていない。突合スクリプトは`tools/verify_render.py`として新設し、リポジトリにコミットする。

### 突合結果（見出し・先頭段落の一致判定）

| URL | mode | 一致判定 | 終了コード | render_status | 早期終了/上限到達 |
|---|---|---|---|---|---|
| https://demo.playwright.dev/todomvc/ | --render | heading=一致 paragraph=比較対象なし | 0 | rendered | 上限到達（real 5.62s、既定`--wait-ms`5000に対し明確に超過） |
| https://demo.playwright.dev/todomvc/ | --auto-render | heading=一致 paragraph=比較対象なし | 0 | rendered | 上限到達（real 5.66s） |
| https://qiita.com/ | --render | heading=不一致 paragraph=不一致（本文抽出が一覧ページに不向き。下の`--raw`行を参照） | 0 | rendered | 上限到達（real 5.68s） |
| https://qiita.com/ | --render --raw | DOMの見出し8件すべてがmarkdownに含まれる | 0 | rendered | 上限到達（real 8.05s） |
| https://qiita.com/ | --auto-render | heading=不一致 paragraph=不一致 | 0 | static | 早期終了（real 0.61s、静的取得のみでrenderへ遷移せず） |
| https://react.dev/learn | --render | heading=一致（`title`フィールド「Quick Start – React」に含まれる） paragraph=一致 | 0 | rendered | 上限到達（real 6.31s） |
| https://react.dev/learn | --auto-render | heading=一致（`title`に含まれる） paragraph=一致 | 0 | static | 早期終了（real 0.28s、静的取得のみでrenderへ遷移せず） |

比較用の文字列は、独立経路の`--dump-dom`から抽出したh1/h2見出し1件と30文字以上の先頭段落1件。抽出結果はいずれも`webgrab-verify/*.dom.html`から実在する要素として取れている。3件とも本文自体は取得できており、`--virtual-time-budget=20000`での再取得は不要と判断した。不一致の内訳は以下の個別事情による。

- `demo.playwright.dev/todomvc/`は空のTodoリストが仕様どおりのDOMで、30文字以上の段落候補が存在しないため、段落側は比較対象なしとして扱った。見出し「todos」はheading一致
- `qiita.com`のトップは記事一覧ページで、本文抽出（readability）は単一記事向けのため一覧の見出しを落とす。SKILLの規則どおり`--render --raw`で取得し直すと、`--dump-dom`から抽出した見出し8件がすべてmarkdownに含まれた（一覧は訪問ごとに入れ替わるため、同じ時刻の取得同士で比較した）。render失敗ではない
- `react.dev/learn`のDOM側`<h1>Quick Start</h1>`は、readability抽出（`dom_smoothie`）が記事タイトルとして`title`フィールドへ移すため本文Markdownには現れない。`tools/verify_render.py`は見出しがmarkdownまたは`title`に含まれれば一致と判定する。段落側は完全一致
- `--render`の3件がいずれも上限到達なのは、todomvcは可視テキストが75文字で早期終了条件（200文字以上）を満たさないため、qiitaとreact.devは解析系の通信が続きネットワーク静止に至らないためである。いずれも設計08 §4.2の待機規則どおりで、上限到達でも本文は取得できている

### Chromeコールドスタート実測とskip閾値の判定

`render_inner`の`Browser::launch`前後に一時的な`eprintln!`を入れ、`./target/release/webgrab https://example.com --render`を5回実行して計測した（作業後に`eprintln!`は削除し、`git diff --stat src/render.rs`が空であることを確認済み）。

- 実測値（ms）: 489、263、324、317、330
- 中央値Lは324ms
- 判定式`5000 >= L + 2000 + 1000`は`5000 >= 3324`となり真
- 結論として、skip閾値5秒は妥当であり、`src/pipeline.rs`の`SKIP_TIMEOUT_MIN`および設計08 §3決定表・§4.3手順2の変更は不要

### CIのsandbox観測

CIの`test`と`coverage`ジョブは`.github/workflows/ci.yml`で最初から`WEBGRAB_E2E_NO_SANDBOX=1`を与えている（設計08 §3の決定）。このフラグ無しでubuntu-24.04ランナーのsandboxが起動するかは未確認である。PR #1のCI（run 33104618349、2026-08-28）では`test`ジョブのE2Eが19件すべて合格し、`coverage`ジョブは除外なしで行カバレッジ92.54%（閾値80）を達成した。`check`（fmt・clippy・doclint・SHAピン留め検証）と`security`（gitleaks）も合格。`actions/checkout`のv4ピンにNode.js 20非推奨の注記が出たため、v7のSHAへ更新した。

### coverageの二択

`cargo llvm-cov ... --fail-under-lines 80`は除外なし（設計08 §7のコマンドそのまま）で91.87%（line coverage）を達成し、設計08 §3決定表・Global Constraint 5の「除外なしで80%達成」を選択した。除外を戻す代替案は不採用（`docs/_quality/IMPROVEMENT_BACKLOG.md`の「実装時の計画逸脱」節に既出）。gzip fixture（`tests/fixtures/big_gzip.html.gz`）は展開後2MiBを`gzip -9`した実測約2.1KiBで、計画が見込んでいた約4KiBより小さい（同ファイルに既出）。

### SKILL経由の呼び出し確認

`cargo install --path .`で更新した`webgrab`と、`samples/skills/claude/webgrab/SKILL.md`を差し替えた`~/.claude/skills/webgrab/SKILL.md`を使い、Claude Codeのセッションから`webgrab-fetch`スキルを起動して、スキル本文が指示するコマンド形を実行した。結果を次に示す（2026-08-27、macOS、Chrome 151）。

| 確認項目 | コマンド | 観測 | 判定 |
|---|---|---|---|
| (a) 既定呼び出し | `webgrab "https://demo.playwright.dev/todomvc/" --format json` | 終了コード0、`render_status=static`、`static_chars=69`、stderrに`warn=short-content chars=93 hint=--render/--raw` | 一致 |
| (b) `--auto-render`のJSON | `webgrab "https://demo.playwright.dev/todomvc/" --auto-render --format json` | 終了コード0、`render_status=rendered`、`static_chars=69`、`rendered_chars=75`、stderrに`info=auto-render reason=short chars=69`と`warn=short-content chars=103 hint=--raw` | 一致 |
| (b) 静的で足りるページ | `webgrab "https://react.dev/learn" --auto-render --format json` | 終了コード0、`render_status=static`、`static_chars=12114`、`rendered_chars=null`、escalationなし | 一致 |
| (c) 終了コード6の`hint=` | ローカルの空シェル（`<div id="app">`をJSで後から埋める）を`webgrab "http://127.0.0.1:18765/index.html" --allow-private --no-robots` | 終了コード6、stderr先頭行`webgrab: error=empty hint=--render/--raw`（直前に`warn=extract-grab-failed`） | スキル本文の「`hint=`が示すフラグを試す」と一致 |
| (c) 同じURLに`--auto-render` | 上記に`--auto-render --format json`を追加 | 終了コード0、`render_status=rendered`、`static_chars=0`、`rendered_chars=460`、本文に`## Late title` | 一致 |
| (d) rendered時の継続コマンド | `webgrab "https://demo.playwright.dev/todomvc/" --auto-render --max-chars 40 --wait-ms 3000` | `[webgrab:truncated chars 0-40 of 103 — continue: webgrab 'https://demo.playwright.dev/todomvc/' --max-chars 40 --render --wait-ms 3000 --start-index 40]`（`--auto-render`は含まれず`--render`と`--wait-ms`が再現） | 一致 |
| (d) 非rendered時の継続コマンド | `webgrab "https://react.dev/learn" --auto-render --wait-ms 3000 --max-chars 100` | `[webgrab:truncated chars 0-100 of 16927 — continue: webgrab 'https://react.dev/learn' --max-chars 100 --start-index 100]`（render系フラグは省略） | 一致 |

補足として、Skillツールが読み込むスキル本文はセッション開始時のキャッシュのため、差し替え後の本文を反映するには新しいセッションが必要である。ディスク上の`~/.claude/skills/webgrab/SKILL.md`は更新版であることを`diff -q`で確認した。実行後にheadless Chromeの残存プロセスは0件だった。

## 取得忠実度の改善の検証（2026-09-21）

設計10の完了条件に沿って、リリースビルドのバイナリで実URLと実テストを検証した。

### V1〜V5

| # | コマンド | 変更前 | 変更後 |
|---|---|---|---|
| V1 | `webgrab https://raw.githubusercontent.com/tokio-rs/tokio/master/tokio/src/sync/mutex.rs --max-chars 10000000 --no-tokens` | 1396行が1行に潰れ、`Mutex<T>`が17個から0個、`<T: ?Sized>`が12個から0個 | 既定・`--raw`・`--format text`・`--format html`のいずれもcurlの取得結果とバイト一致（1396行、`Mutex<T>` 17個、`<T: ?Sized>` 12個） |
| V2 | `webgrab https://docs.rs/tokio --render --max-chars 200000` | `URL Source`が`https://docs.rs/tokio`のままで、本文先頭6本のdocs.rsリンク中5本がHTTP 400 | `URL Source`が`https://docs.rs/tokio/latest/tokio/`になり、先頭6本すべてがHTTP 200 |
| V3 | `webgrab https://crates.io/crates/tokio --auto-render --max-chars 0` | HTTP 404で終了コード4 | 終了コード0で総文字数8941文字（静的フェーズがHTTP 200と空シェルを取得したのちエスカレーションする） |
| V4 | `webgrab https://doc.rust-lang.org/book/ch03-02-data-types.html --format text --max-chars N`（Nは1500から900刻みで21通り） | 行の途中で切れた回数21回中18回 | 行の途中で切れた回数0回。N=2400では継続コマンドを最後まで実行すると8ページに分かれ、最終ページを除く7ページすべてが改行で終わり、連結が`--max-chars 10000000`の本文（17250文字）と一致 |
| V5 | `webgrab https://stackoverflow.com/questions/27535289/what-is-the-correct-way-to-return-an-iterator` | 終了コード4、stderrに再試行の手掛かりなし | 終了コード4、stderr先頭行`webgrab: error=http HTTP 403 retryable=false hint=--render`。`--auto-render`を付けてもエスカレーションしない。404には`hint=`が付かない |

### テストとカバレッジ

- `cargo test --lib --bins --test integration`はunit 188件とintegration 24件がすべてpassし終了コード0
- `WEBGRAB_E2E=1 cargo test --test render_e2e -- --test-threads=1`は24件がすべてpass（設計10のE16〜E20を含む）
- `cargo crap --lcov lcov.info --min 30`は、E2Eを含む`cargo llvm-cov`実行後の計測でCRAP値30以上の関数がゼロ件。分割前後の代表値は、`pipeline::run`が循環的複雑度33からCRAP 9.0（分割後の複雑度9）へ、`fetch::fetch`がCRAP 44.3から22.3へ、`fetch::robots_precheck`がCRAP 30.0から5.9へ低下した

### ミューテーションテスト

`git diff master -- src`の差分を`cargo mutants --in-diff`に渡し、変更行のミュータントだけを対象にした。Chromeを必要としないファイルは既定の`cargo test`で、`src/render.rs`と`src/render/world.rs`は`WEBGRAB_E2E=1`を付けて`--lib --test render_e2e`と`--test-threads=1`で実行した。

| 対象 | ミュータント数 | 検出 | タイムアウト | ビルド不能 | 生存 |
|---|---|---|---|---|---|
| `budget.rs`・`convert.rs`・`fetch.rs`・`pipeline.rs` | 94 | 75 | 7 | 12 | 0 |
| `decode.rs` | 10 | 8 | 0 | 2 | 0 |
| `render.rs`・`render/world.rs`（E2E込み） | 19 | 18 | 0 | 1 | 0 |
| `convert.rs`の制御文字を読み飛ばすスキーム判定 | 7 | 7 | 0 | 0 | 0 |

タイムアウトの7個は、いずれも`convert::sanitize_link_schemes`の走査位置の更新を壊して無限ループにするミュータントで、テストが終了しないことにより検出される。生存したミュータントはない。
