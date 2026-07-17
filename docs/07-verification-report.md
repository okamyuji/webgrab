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
- 判定: 合格。**WebFetchが100,000文字で切断するのに対し、webgrabは455,255文字全体を認識し、指定量で切り出し、続きの取得コマンドを自己提示した**（Zenn記事の主張する制約を解消できることの実証）

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

- robots.txt取得URLがポート番号を欠落しており、非標準ポートのサーバで robots が常に「許可」扱いになる不具合を統合テスト `robots_disallow_returns_exit_5` が検出。`fetch.rs` のauthority構築を修正して解消。
- 実装レビュー（rust-reviewer）がCRITICAL C1（robots.txt取得がreqwest自動リダイレクトでnetguard未検証＝SSRFバイパス）を検出。手動1回追従+追従先再検証に修正し、回帰テスト `robots_redirect_is_manually_followed_once` を追加して緑を確認。HIGH 3件（async内同期DNS、継続コマンドのフラグ取りこぼし、renderの一時プロファイル未削除）も修正済み。詳細は [_quality/IMPROVEMENT_BACKLOG.md](_quality/IMPROVEMENT_BACKLOG.md)。

## 実装レビュー修正後の再検証（2026-07-17）

- Ran: `webgrab https://example.com --render --max-chars 300` → Exit 0 → Observed: 本文取得成功。一時プロファイルディレクトリ `webgrab-chrome-*` の残留を確認したところ0件（H3のTempDir RAII削除が実機で機能）。
- Ran: `cargo test` → Exit 0 → Observed: 66件pass（C1回帰テスト含む）。

## 総合判定

設計§9の完了条件1〜5をすべて観測エビデンス付きで満たした。webgrabは、Zenn記事が指摘した「LLMのWeb取得は先頭しか読まない／要約しか得られない」という制約を、実URLで実際に解消できることを実証した。
