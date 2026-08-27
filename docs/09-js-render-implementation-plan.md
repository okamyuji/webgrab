# JS描画ページ取得改善 実装計画書

> For agentic workers: REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

Goal:設計書[08-js-render-design.md](08-js-render-design.md) v1.6を実装し、`--auto-render`・待機戦略・展開後バイト上限・E2E・CIをPRとして提出する。

Architecture:既存のpipelineを「静的フェーズ → エスカレーション判定 → renderフェーズ → 出力」に再構成し、判定はすべてChrome非依存の純関数（`pipeline.rs` / `render/wait.rs` / `convert.rs`）に置く。`render.rs`はCDPイベント監視（in-flight集合・展開後バイト）、分離ワールドでのevaluate、メインフレーム限定の`main_blocked`、deadline基準の待機を実装し、終了コード8を単一経路で判定する。E2EはローカルHTTPサーバ + 実Chromeで回し、CIは`ubuntu-24.04`に固定する。

Tech Stack: Rust 2024 edition、tokio、chromiumoxide 0.9.1（CDP: Fetch / Network / Page.createIsolatedWorld / Runtime.evaluate）、dom_smoothie 0.18、htmd、clap 4、GitHub Actions。

Spec: `docs/08-js-render-design.md` v1.6（§4仕様、§5モジュール変更、§6テスト、§7 CI、§8検証、§9完了条件）。実装者は本計画と設計書の両方を読む。

## Global Constraints

設計書 §9の完了条件を逐語で写す。すべてのタスクがこれに拘束される。

1. `cargo test`が終了コード0（E2Eは`WEBGRAB_E2E=1`付きでローカルでも0）
2. `cargo clippy --all-targets -- -D warnings`が終了コード0
3. `python3 tools/doclint.py docs/`が`Critical 0 / High 0`
4. §6 E1〜E9、E10a、E10b、E11〜E15がCIの`test`ジョブで実行され（skipでなく）すべて合格
5. `cargo llvm-cov ... --fail-under-lines 80`が終了コード0。除外なし（§7のコマンドそのまま）で達成するか、`--ignore-filename-regex 'render\.rs'`を戻して達成し差分をバックログに記録するかの二択で、どちらを採ったかを07-verification-report.mdに記録する
6. §4.2・§4.3で追加する各機構に対応する単体テストまたはE2Eが存在する（`InFlight`・`should_stop`・`is_main_navigation`・`remaining_budget`・`choose_result`・`fallback_reason`・`hint_for`・`visible_text_len`は単体、skip契約は統合テスト、他はE1〜E15）
7. §8の突合結果（Chromeコールドスタートの実測Lを含み、skip閾値5秒が「L + 2000ms + 1000ms」以上であることを確認。早期終了したか上限到達だったかも記録）が07-verification-report.mdに記録されている
8. PRのCIがすべて緑（CodeRabbitの指摘への対応は人手の完了条件として別途扱う）
9. `.github/workflows/ci.yml`の全`uses:`が40桁のコミットSHAで、`actions/checkout`に`persist-credentials: false`がある（checkジョブの`grep`で機械検証）

追加の固定値（設計書から逐語）:短文閾値200文字/ `DEFAULT_WAIT_MS = 5000` / `POLL_MS = 250` / `STABLE_POLLS = 2`（直前との一致回数）/ `TOMBSTONE_MS = 2000` /集合上限4096 / skip閾値 残余5秒・256KiB / `wait_for_navigation`上限`min(1000ms,残り)` / `content()`予備2000ms / intercept同期待ち500ms / intercept同時実行16 /ホスト解決上限2秒/ stderr詳細行512バイト（文字境界）/ CI runner `ubuntu-24.04`。

## Codexでの実行環境（この計画をCodexへ直接渡す場合）

- 起動は`codex --sandbox danger-full-access -a never`相当で行う（Chromeの子プロセス起動、`gh`、`git push`、外部HTTPが必要）。作業ディレクトリは`/Users/yujiokamoto/devs/rust/llm-web-fetch`。
- 環境変数`WEBGRAB_E2E=1`をセットして起動する。ChromeはPATH上またはchromiumoxideの既定検出（macOSは`/Applications/Google Chrome.app`）で見つかる。見つからなければ`WEBGRAB_CHROME=<実行ファイルパス>`を与える。
- 最初に前提を確認する。`gh auth status`、`"/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" --version`（Linuxは`google-chrome --version`）、`cargo llvm-cov --version`（無ければ`cargo install cargo-llvm-cov`）。
- Claude Code固有のツール（`claude-in-chrome`等）は使えない。ブラウザ突合はTask 14の手順（headless Chromeの`--dump-dom`）で行う。
- 各Taskは順番どおりに実施し、Stepのチェックボックスを埋めながら進める。Taskごとにコミットする。
- 判断に迷う箇所は設計書`docs/08-js-render-design.md`（v1.6）の該当節を正とし、設計から逸脱する変更は行わない。設計に無い挙動が必要になった場合は`docs/_quality/IMPROVEMENT_BACKLOG.md`に理由を記録してから最小の変更に留める。

コミット規約: conventional commits。`Claude-Session:`行は付けない。`Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`を付ける。作業ブランチ`feat/js-render-auto`（master直コミット禁止）。

---

## ファイル構成

| ファイル | 責務 | 状態 |
|---|---|---|
| `src/error.rs` | `WebgrabError`にトークン（`hint=`等）と詳細行サニタイズを追加 | 変更 |
| `src/budget.rs` | `shell_quote`を`pub(crate)`に | 変更 |
| `src/convert.rs` | `visible_text_len` | 変更 |
| `src/extract.rs` | `GrabFailed` → 空本文 + `warn=extract-grab-failed` | 変更 |
| `src/output.rs` | `RenderStatus`、`Meta`拡張、JSON/frontmatterフィールド、`[webgrab:render-status ...]`行、htmlの`-->`ガード | 変更 |
| `src/cli.rs` | `--auto-render`、`--no-sandbox`、`--wait-ms: Option<u64>`、`extra_flags(cli, status)` | 変更 |
| `src/render/wait.rs` | `InFlight`、`DecodedBudget`、`should_stop`、`is_main_navigation`、定数 | 新設 |
| `src/renderproxy.rs` | `HostCache`（2秒上限・fail-closed・共有）、遮断件数 | 変更 |
| `src/render.rs` | §4.2の待機、`main_blocked`単一経路、分離ワールド、deadline、`no_sandbox` | 変更 |
| `src/fetch.rs` | `Fetched.consumed_bytes` | 変更 |
| `src/pipeline.rs` | 3フェーズ再構成、純関数群、`flag-ignored` | 変更 |
| `tests/common/mod.rs` | 常駐HTTPサーバ（バイト本文・任意ヘッダ・遅延）、E2Eゲート、`E2E_LOCK` | 新設 |
| `tests/fixtures/big_gzip.html.gz`, `tests/fixtures/README.md` | gzip fixtureと生成手順 | 新設 |
| `tests/integration.rs` | F1回帰、`hint=`、`flag-ignored`、skip契約 | 変更 |
| `tests/render_e2e.rs` | E1〜E15 | 新設 |
| `.github/workflows/ci.yml` | §7 | 新設 |
| `docs/04-design.md` v1.3、`README.md`、`samples/skills/*`、`docs/07-verification-report.md`、`docs/_quality/IMPROVEMENT_BACKLOG.md` | 文書 | 変更 |

---

### Task 0:ブランチ作成と設計文書のコミット

Files:
- Commit: `docs/08-js-render-design.md`, `docs/09-js-render-implementation-plan.md`, `docs/_quality/*.md`

- [ ] Step 1:ブランチを切る

```bash
git -C /Users/yujiokamoto/devs/rust/llm-web-fetch checkout -b feat/js-render-auto
```

- [ ] Step 2:ベースラインが緑であることを確認

Run: `cargo test 2>&1 | grep 'test result'`
Expected:全行`ok`（単体108、統合9）

- [ ] Step 3:設計文書をコミット

```bash
git add docs/08-js-render-design.md docs/09-js-render-implementation-plan.md docs/_quality/QUALITY_RUBRIC.md docs/_quality/SELF_REVIEW_LOG.md docs/_quality/IMPROVEMENT_BACKLOG.md
git commit -m "docs: JS描画ページ取得改善の設計書v1.6と実装計画を追加

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 1: error.rs — stderrトークンと詳細行サニタイズ

Files:
- Modify: `src/error.rs`
- Modify: `src/budget.rs:70`（`shell_quote`を`pub(crate)`）

Interfaces:
- Produces: `WebgrabError::with_token(self, key: &'static str, value: impl Into<String>) -> Self`（先頭行に`key=value`を追加。トークン付きの場合、先頭行は`webgrab: error=<code> key=value ...`でメッセージを含めず、メッセージは詳細の1行目に出す）、`pub fn sanitize_detail(s: &str) -> String`（C0/DEL/C1除去、`\t\n\r` U+2028 U+2029を空白に、512バイト以内の文字境界で切り詰め`…`）、`pub(crate) fn budget::shell_quote`

- [ ] Step 1:失敗するテストを書く（`src/error.rs`のtestsに追加）

```rust
    #[test]
    fn token_moves_message_to_detail_line() {
        let e = WebgrabError::new(ExitCode::Empty, "empty body extracted")
            .with_token("hint", "--render/--raw");
        let lines = e.stderr_lines();
        assert_eq!(lines[0], "webgrab: error=empty hint=--render/--raw");
        assert_eq!(lines[1], "empty body extracted");
    }

    #[test]
    fn without_token_first_line_keeps_message() {
        let e = WebgrabError::new(ExitCode::Http, "HTTP 503 retryable=true");
        let lines = e.stderr_lines();
        assert_eq!(lines[0], "webgrab: error=http HTTP 503 retryable=true");
        assert_eq!(lines.len(), 1);
    }

    #[test]
    fn detail_is_sanitized_to_one_line_and_capped() {
        let raw = format!("a\nb\u{1b}[31mc\u{2028}d{}", "é".repeat(600));
        let d = sanitize_detail(&raw);
        assert!(!d.contains('\n') && !d.contains('\u{1b}') && !d.contains('\u{2028}'));
        assert!(d.starts_with("a b c d"));
        assert!(d.len() <= 512 + '…'.len_utf8(), "len={}", d.len());
        assert!(d.ends_with('…'));
        assert!(d.is_char_boundary(d.len() - '…'.len_utf8()));
    }
```

- [ ] Step 2:失敗を確認

Run: `cargo test --lib error::tests -- --nocapture`
Expected:コンパイルエラー（`with_token` / `stderr_lines` / `sanitize_detail`未定義）

- [ ] Step 3:実装

`src/error.rs`の`WebgrabError`とimplを次に置き換える。

```rust
/// webgrabのエラー。終了コードと人間可読メッセージを持つ。
#[derive(Debug)]
pub struct WebgrabError {
    pub code: ExitCode,
    pub message: String,
    /// 追加の診断行（stderr 2行目以降、空白を含んでよい）。
    pub detail: Option<String>,
    /// 先頭行に付加する機械可読トークン（例: `hint=--render/--raw`）。
    pub tokens: Vec<(&'static str, String)>,
}

const DETAIL_MAX_BYTES: usize = 512;

/// stderrの詳細行を1行に畳む。C0/DEL/C1制御文字は除去し、改行・タブ・行区切りは空白へ。
/// 512バイトを超えない最大の文字境界で切り詰めて `…` を付す。
pub fn sanitize_detail(s: &str) -> String {
    let mut out = String::with_capacity(s.len().min(DETAIL_MAX_BYTES + 4));
    for c in s.chars() {
        let c = match c {
            '\t' | '\n' | '\r' | '\u{2028}' | '\u{2029}' => ' ',
            '\u{0}'..='\u{1F}' | '\u{7F}'..='\u{9F}' => continue,
            c => c,
        };
        if out.len() + c.len_utf8() > DETAIL_MAX_BYTES {
            out.push('…');
            return out;
        }
        out.push(c);
    }
    out
}

impl WebgrabError {
    pub fn new(code: ExitCode, message: impl Into<String>) -> Self {
        Self { code, message: message.into(), detail: None, tokens: Vec::new() }
    }

    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(sanitize_detail(&detail.into()));
        self
    }

    pub fn with_token(mut self, key: &'static str, value: impl Into<String>) -> Self {
        self.tokens.push((key, value.into()));
        self
    }

    /// stderrへ出す行の列。トークン付きなら先頭行はトークンのみで、メッセージは2行目。
    pub fn stderr_lines(&self) -> Vec<String> {
        let mut lines = Vec::new();
        if self.tokens.is_empty() {
            lines.push(format!("webgrab: error={} {}", self.code.token(), self.message));
        } else {
            let toks: Vec<String> = self.tokens.iter().map(|(k, v)| format!("{k}={v}")).collect();
            lines.push(format!("webgrab: error={} {}", self.code.token(), toks.join(" ")));
            lines.push(self.message.clone());
        }
        if let Some(d) = &self.detail {
            lines.push(d.clone());
        }
        lines
    }

    /// stderrへ機械可読書式で出力する。
    pub fn print_stderr(&self) {
        for l in self.stderr_lines() {
            eprintln!("{l}");
        }
    }
}
```

`src/budget.rs:70`を`pub(crate) fn shell_quote(s: &str) -> String`に変更する。

- [ ] Step 4:テストが通ることを確認

Run: `cargo test --lib error::tests`
Expected: PASS（既存3件 + 新規3件）。`cargo test`全体も緑（`with_detail`の引数型は変えていない）。

- [ ] Step 5:コミット

```bash
git add src/error.rs src/budget.rs
git commit -m "feat(error): stderr先頭行トークンと詳細行のサニタイズを追加

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 2: convert.rs — `visible_text_len`

Files:
- Modify: `src/convert.rs`

Interfaces:
- Produces: `pub fn visible_text_len(html: &str) -> usize`（`script`/`style`/`noscript`を要素ごと除去 → タグ除去 → HTML実体`&amp;` `&lt;` `&gt;` `&quot;` `&#39;` `&nbsp;`を1文字に → 空白畳み込み → trim → `chars().count()`。失敗しない）

- [ ] Step 1:失敗するテストを書く

```rust
    #[test]
    fn visible_text_len_ignores_urls_and_scripts() {
        // 30文字超のhrefを持つリンク10個。アンカーテキスト2文字×10=20だけが数えられる。
        let nav: String = (0..10)
            .map(|i| format!("<a href=\"https://example.com/very/long/path/segment/{i:04}/page.html\">ホーム</a>"))
            .collect();
        let html = format!("<html><head><style>p{{}}</style><script>var x='xxxxxxxxxx';</script></head><body><nav>{nav}</nav><div id=\"app\"></div></body></html>");
        assert_eq!(visible_text_len(&html), 20);
    }

    #[test]
    fn visible_text_len_counts_article_text_and_entities() {
        let html = "<article><h1>見出し</h1><p>本文&amp;続き&nbsp;末尾</p></article>";
        // 見出し(3) + 本文&続き 末尾(7) = 10。タグ境界は空白1つに畳まれ、trimされる。
        assert_eq!(visible_text_len(html), 3 + 1 + 7);
        assert_eq!(visible_text_len(""), 0);
        assert_eq!(visible_text_len("<div id=\"app\"></div>"), 0);
    }
```

- [ ] Step 2:失敗を確認

Run: `cargo test --lib convert::tests::visible_text_len`
Expected:コンパイルエラー（未定義）

- [ ] Step 3:実装

```rust
/// 可視テキストの文字数（Unicodeスカラー値）。エスカレーション判定と`static_chars`/`rendered_chars`に使う。
/// script/style/noscriptを要素ごと除去し、タグを落とし、代表的な実体参照を1文字に戻し、
/// 空白を畳んでtrimする。リンク先や画像URLは含まない。失敗しない。
pub fn visible_text_len(html: &str) -> usize {
    let stripped = strip_non_content(html);
    let mut text = String::with_capacity(stripped.len());
    let mut in_tag = false;
    for c in stripped.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => {
                in_tag = false;
                text.push(' ');
            }
            _ if in_tag => {}
            c => text.push(c),
        }
    }
    let text = text
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'");
    let mut count = 0usize;
    let mut prev_space = true;
    let mut pending_space = false;
    for c in text.chars() {
        if c.is_whitespace() {
            if !prev_space {
                pending_space = true;
            }
            prev_space = true;
        } else {
            if pending_space {
                count += 1;
                pending_space = false;
            }
            count += 1;
            prev_space = false;
        }
    }
    count
}
```

- [ ] Step 4:テストが通ることを確認

Run: `cargo test --lib convert::tests`
Expected: PASS

- [ ] Step 5:コミット

```bash
git add src/convert.rs
git commit -m "feat(convert): 可視テキスト長 visible_text_len を追加

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 3: extract.rs — `GrabFailed`を空本文へ写像

Files:
- Modify: `src/extract.rs:88-90`

- [ ] Step 1:失敗するテストを書く

```rust
    #[test]
    fn empty_shell_maps_to_empty_body_not_error() {
        // JSで本文を後から入れる空シェル。dom_smoothieはGrabFailedを返すが、Ok(空)にする（設計§4.1）。
        let e = extract("<html><body><div id=\"app\"></div></body></html>", "https://x.test").unwrap();
        assert!(e.content_html.trim().is_empty());
    }

    #[test]
    fn placeholder_text_is_extracted_as_short_body() {
        // F2の再現markup。8文字が抽出される（E7の前提を固定）。
        let e = extract("<html><body><div id=\"app\">読み込み中...</div></body></html>", "https://x.test").unwrap();
        let text = crate::convert::to_text(&e.content_html).unwrap();
        assert_eq!(text.trim(), "読み込み中...");
    }
```

- [ ] Step 2:失敗を確認

Run: `cargo test --lib extract::tests`
Expected: `empty_shell_maps_to_empty_body_not_error`が`unwrap`でpanic

- [ ] Step 3:実装（`extract`の`parse`部分を置換）

```rust
    let article = match readability.parse() {
        Ok(a) => a,
        Err(dom_smoothie::ReadabilityError::GrabFailed) => {
            // 本文が見つからない＝空本文。pipelineの空本文チェック（終了コード6）へ委ねる。
            eprintln!("webgrab: warn=extract-grab-failed");
            return Ok(Extracted::default());
        }
        Err(e) => {
            return Err(WebgrabError::new(ExitCode::Internal, "readability parse failed")
                .with_detail(e.to_string()));
        }
    };
```

`ReadabilityError`の変種名は`~/.cargo/registry/src/*/dom_smoothie-0.18.0/src/lib.rs:27-34`で確認する（`GrabFailed`はユニットvariant）。

- [ ] Step 4:確認

Run: `cargo test --lib extract::tests`
Expected: PASS

- [ ] Step 5:コミット

```bash
git add src/extract.rs
git commit -m "fix(extract): 本文なし(GrabFailed)を内部エラーでなく空本文として返す

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 4: output.rs — `RenderStatus`、メタ拡張、render-status行

Files:
- Modify: `src/output.rs`

Interfaces:
- Produces:
```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderStatus { Static, Rendered, Failed(&'static str), NoGain, Skipped(&'static str) }
impl RenderStatus {
    pub fn label(self) -> &'static str;   // "static" | "rendered" | "failed" | "no-gain" | "skipped"
    pub fn reason(self) -> Option<&'static str>; // Failed(r)/Skipped(r) → Some(r), NoGain → Some("shorter"), 他 None
    pub fn is_rendered(self) -> bool;     // Rendered のみ true
}
```
  `Meta`に`pub render_status: RenderStatus`（`Default`は`Static`）、`pub static_chars: Option<usize>`、`pub rendered_chars: Option<usize>`を追加。

- [ ] Step 1:失敗するテストを書く（既存`meta()`ヘルパは`..Default::default()`を足してから拡張）

```rust
    fn meta() -> Meta {
        Meta {
            title: Some("T".into()),
            url: "https://x.test".into(),
            published_time: Some("2026-01-01T00:00:00Z".into()),
            tokens: Some(42),
            short_content: None,
            short_content_suggest: "",
            fence: false,
            ..Default::default()
        }
    }

    #[test]
    fn json_and_frontmatter_carry_render_status_and_char_counts() {
        let mut m = meta();
        m.render_status = RenderStatus::NoGain;
        m.static_chars = Some(150);
        m.rendered_chars = Some(20);
        let s = slc("body", false, false, 4);
        let js = render(Format::Json, &m, &s, false, &[]);
        let v: serde_json::Value = serde_json::from_str(&js).unwrap();
        assert_eq!(v["render_status"], "no-gain");
        assert_eq!(v["static_chars"], 150);
        assert_eq!(v["rendered_chars"], 20);
        let fm = render(Format::Frontmatter, &m, &s, false, &[]);
        assert!(fm.contains("render_status: \"no-gain\""), "{fm}");
        // static では rendered_chars は null
        let mut m2 = meta();
        m2.static_chars = Some(300);
        let js2 = render(Format::Json, &m2, &s, false, &[]);
        let v2: serde_json::Value = serde_json::from_str(&js2).unwrap();
        assert_eq!(v2["render_status"], "static");
        assert!(v2["rendered_chars"].is_null());
    }

    #[test]
    fn render_status_line_only_for_anomalies_and_after_other_markers() {
        let mut m = meta();
        m.render_status = RenderStatus::Failed("render");
        m.short_content = Some(42);
        m.short_content_suggest = "--raw";
        m.fence = true;
        let s = slc("short", true, false, 42);
        let md = render(Format::Markdown, &m, &s, false, &[]);
        let i_fence = md.find(FENCE_CLOSE).unwrap();
        let i_trunc = md.find("[webgrab:truncated").unwrap();
        let i_short = md.find("[webgrab:short-content").unwrap();
        let i_rs = md.find("[webgrab:render-status failed reason=render]").unwrap();
        assert!(i_fence < i_trunc && i_trunc < i_short && i_short < i_rs, "{md}");
        // rendered / static では出ない
        for st in [RenderStatus::Static, RenderStatus::Rendered] {
            let mut m3 = meta();
            m3.render_status = st;
            let out = render(Format::Markdown, &m3, &s, false, &[]);
            assert!(!out.contains("render-status"), "{out}");
        }
        // html はコメント、直前に閉じ忘れ対策の --> が出る
        let html = render(Format::Html, &m, &s, false, &[]);
        assert!(html.contains("-->\n<!-- [webgrab:render-status failed reason=render] -->"), "{html}");
        // --max-chars 0 の text でも出る
        let txt = render(Format::Text, &m, &slc("", false, false, 42), true, &[]);
        assert!(txt.contains("[webgrab:meta-only total 42 chars]\n[webgrab:render-status failed reason=render]"), "{txt}");
    }
```

- [ ] Step 2:失敗を確認

Run: `cargo test --lib output::tests`
Expected:コンパイルエラー（`RenderStatus`未定義）

- [ ] Step 3:実装

`Meta`の直前に追加を次に示す。

```rust
/// 取得経路の状態（設計§4.4）。JSON/frontmatterの`render_status`と自己記述行に使う。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RenderStatus {
    #[default]
    Static,
    Rendered,
    Failed(&'static str),
    NoGain,
    Skipped(&'static str),
}

impl RenderStatus {
    pub fn label(self) -> &'static str {
        match self {
            RenderStatus::Static => "static",
            RenderStatus::Rendered => "rendered",
            RenderStatus::Failed(_) => "failed",
            RenderStatus::NoGain => "no-gain",
            RenderStatus::Skipped(_) => "skipped",
        }
    }
    pub fn reason(self) -> Option<&'static str> {
        match self {
            RenderStatus::Failed(r) | RenderStatus::Skipped(r) => Some(r),
            RenderStatus::NoGain => Some("shorter"),
            _ => None,
        }
    }
    pub fn is_rendered(self) -> bool {
        self == RenderStatus::Rendered
    }
    /// 既定形式のstdoutに付ける自己記述行。failed/no-gain/skippedのときだけ。
    fn marker(self) -> Option<String> {
        match self.reason() {
            Some(r) => Some(format!("[webgrab:render-status {} reason={r}]", self.label())),
            None => None,
        }
    }
}
```

`Meta`にフィールドを追加を次に示す。

```rust
    /// 取得経路の状態（設計§4.4）。
    pub render_status: RenderStatus,
    /// 静的フェーズの可視テキスト長（`--render`明示時はNone）。
    pub static_chars: Option<usize>,
    /// renderフェーズの可視テキスト長（renderしてDOMを得たときのみSome）。
    pub rendered_chars: Option<usize>,
```

`render_markdown`の末尾（short-contentの後）と`render_plain`の末尾にを次に示す。

```rust
    if let Some(mk) = meta.render_status.marker() {
        out.push('\n');
        out.push_str(&mk);
    }
```

`render_plain`はhtmlのとき、マーカー群（footer / short-content / render-statusのいずれかを出す前）に1度だけ`-->`ガードを出す。`render_plain`を次に置換を次に示す。

```rust
fn render_plain(
    fmt: Format,
    meta: &Meta,
    slice: &Slice,
    max_chars_zero: bool,
    extra_flags: &[String],
) -> String {
    let is_html = fmt == Format::Html;
    let mut out = String::new();
    let mut markers: Vec<String> = Vec::new();

    if max_chars_zero {
        out.push_str(&wrap_marker(&format!("[webgrab:meta-only total {} chars]", slice.total), is_html));
    } else {
        let url = sanitize_line(&meta.url);
        out.push_str(&fenced_body(&slice.content, &url, meta.fence, is_html));
        if let Some(f) = footer(meta, slice, extra_flags) {
            markers.push(f);
        }
        if let Some(total) = meta.short_content {
            markers.push(budget::short_content_marker(total, meta.short_content_suggest));
        }
    }
    if let Some(mk) = meta.render_status.marker() {
        markers.push(mk);
    }
    if !markers.is_empty() && is_html && !max_chars_zero {
        // 本文側の閉じ忘れ <!-- にマーカーが飲み込まれないよう、先に1つ閉じる。
        out.push_str("\n-->");
    }
    for mk in markers {
        out.push('\n');
        out.push_str(&wrap_marker(&mk, is_html));
    }
    out
}
```

`render_markdown`のfrontmatterブロックに`truncated:`の後でを次に示す。

```rust
        out.push_str(&format!("render_status: {}\n", yaml_scalar(meta.render_status.label())));
```

`render_json`の`json!`に追加を次に示す。

```rust
        "render_status": meta.render_status.label(),
        "static_chars": meta.static_chars,
        "rendered_chars": meta.rendered_chars,
```

- [ ] Step 4:確認

Run: `cargo test --lib output::tests`
Expected: PASS（既存テストは`..Default::default()`で維持）。`text_max_chars_zero_meta_only`の期待`"[webgrab:meta-only total 999 chars]"`は`render_status=Static`のとき変わらないので通る。

- [ ] Step 5:コミット

```bash
git add src/output.rs
git commit -m "feat(output): render_statusとstatic/rendered_chars、自己記述行を追加

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 5: cli.rs — フラグ追加と継続コマンド規則

Files:
- Modify: `src/cli.rs`

Interfaces:
- Consumes: `output::RenderStatus`、`budget::shell_quote`
- Produces: `pub const DEFAULT_WAIT_MS: u64 = 5000;`、`Cli.auto_render: bool`、`Cli.no_sandbox: bool`、`Cli.wait_ms: Option<u64>`、`pub fn extra_flags(cli: &Cli, status: RenderStatus) -> Vec<String>`

- [ ] Step 1:失敗するテストを書く（既存`extra_flags_excludes_start_index_and_output`は`extra_flags(&cli, RenderStatus::Rendered)`に変更）

```rust
    use crate::output::RenderStatus;

    #[test]
    fn auto_render_is_replaced_by_render_only_when_rendered() {
        let cli = Cli::try_parse_from(["webgrab", "https://x.test", "--auto-render", "--no-sandbox", "--wait-ms", "3000"]).unwrap();
        let r = extra_flags(&cli, RenderStatus::Rendered);
        assert_eq!(r.iter().filter(|s| *s == "--render").count(), 1);
        assert!(!r.iter().any(|s| s == "--auto-render"));
        assert!(r.contains(&"--no-sandbox".to_string()));
        assert!(r.contains(&"--wait-ms 3000".to_string()));
        for st in [RenderStatus::Static, RenderStatus::NoGain, RenderStatus::Failed("render"), RenderStatus::Skipped("timeout")] {
            let f = extra_flags(&cli, st);
            assert!(!f.iter().any(|s| s.contains("render")), "{st:?}: {f:?}");
            assert!(!f.iter().any(|s| s.contains("sandbox") || s.contains("wait-ms")), "{st:?}: {f:?}");
        }
    }

    #[test]
    fn explicit_render_with_auto_render_emits_render_once() {
        let cli = Cli::try_parse_from(["webgrab", "https://x.test", "--render", "--auto-render"]).unwrap();
        let f = extra_flags(&cli, RenderStatus::Rendered);
        assert_eq!(f.iter().filter(|s| *s == "--render").count(), 1);
    }

    #[test]
    fn wait_ms_default_is_not_reproduced_but_explicit_is() {
        let d = Cli::try_parse_from(["webgrab", "https://x.test", "--render"]).unwrap();
        assert_eq!(d.wait_ms, None);
        assert!(!extra_flags(&d, RenderStatus::Rendered).iter().any(|s| s.contains("wait-ms")));
        let e = Cli::try_parse_from(["webgrab", "https://x.test", "--render", "--wait-ms", "2000"]).unwrap();
        assert!(extra_flags(&e, RenderStatus::Rendered).contains(&"--wait-ms 2000".to_string()));
        let same = Cli::try_parse_from(["webgrab", "https://x.test", "--render", "--wait-ms", "5000"]).unwrap();
        assert!(extra_flags(&same, RenderStatus::Rendered).contains(&"--wait-ms 5000".to_string()));
    }

    #[test]
    fn value_flags_are_shell_quoted() {
        let cli = Cli::try_parse_from(["webgrab", "https://x.test", "--render", "--user-agent", "a'; id; #", "--chrome-path", "/opt/x y"]).unwrap();
        let f = extra_flags(&cli, RenderStatus::Rendered);
        assert!(f.contains(&r"--user-agent 'a'\''; id; #'".to_string()), "{f:?}");
        assert!(f.contains(&"--chrome-path '/opt/x y'".to_string()), "{f:?}");
    }
```

- [ ] Step 2:失敗を確認

Run: `cargo test --lib cli::tests`
Expected:コンパイルエラー

- [ ] Step 3:実装

`Cli`の変更を次に示す。

```rust
    /// --render / --auto-render時、goto開始からDOM取得までの上限ミリ秒（既定5000）
    #[arg(long)]
    pub wait_ms: Option<u64>,

    /// 静的取得の本文が空または200文字未満のとき、同一プロセス内でJSレンダリングに切り替える
    #[arg(long, default_value_t = false)]
    pub auto_render: bool,

    /// Chromeのsandboxを無効化する（sandboxが起動しないCI環境向け。通常は指定しない）
    #[arg(long, default_value_t = false)]
    pub no_sandbox: bool,
```

定数と`extra_flags`:

```rust
/// --wait-msの既定値（設計§4.4）。
pub const DEFAULT_WAIT_MS: u64 = 5000;

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
```

`EXIT_CODE_HELP`の6の行を`6  empty body (0 chars extracted; see hint= on stderr)`に変える。

- [ ] Step 4:確認

Run: `cargo test --lib cli::tests`
Expected: PASS。`pipeline.rs`の`cli::extra_flags(cli)`呼び出しがコンパイルエラーになるので、一時的に`cli::extra_flags(cli, output::RenderStatus::Static)`に直す（Task 10で正しく置換）。`cli.wait_ms`を使う`pipeline.rs:48`は`cli.wait_ms.unwrap_or(cli::DEFAULT_WAIT_MS)`に直す。

- [ ] Step 5:コミット

```bash
git add src/cli.rs src/pipeline.rs
git commit -m "feat(cli): --auto-render/--no-sandboxを追加し継続コマンドの置換規則を実装

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 6: render/wait.rs — 純関数群

Files:
- Create: `src/render/wait.rs`
- Modify: `src/render.rs`（先頭に`pub mod wait;`）

Interfaces:
- Produces:
```rust
pub const POLL_MS: u64 = 250;
pub const STABLE_POLLS: u32 = 2;
pub const TOMBSTONE_MS: u64 = 2000;
pub const MAX_TRACKED: usize = 4096;
pub const MIN_TEXT_CHARS: usize = 200;
pub struct InFlight { .. }  // new(), on_request(&mut self, id: &str, is_redirect: bool, now: Instant), on_done(&mut self, id: &str, now: Instant), is_idle(&mut self, now: Instant) -> bool, len()
pub struct DecodedBudget { .. } // new(max: u64), on_data(&self, len: u64) -> bool, exceeded(&self) -> bool, total(&self) -> u64  （AtomicU64/AtomicBool、&self で共有可）
pub fn should_stop(idle: bool, stable_polls: u32, text_len: usize, elapsed: Duration, cap: Duration) -> bool
pub fn is_main_navigation(resource_type: &ResourceType, frame_id: &FrameId, main_frame_id: &FrameId) -> bool
```

- [ ] Step 1:失敗するテストを書く（`src/render/wait.rs`内）

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use chromiumoxide::cdp::browser_protocol::network::ResourceType;
    use chromiumoxide::cdp::browser_protocol::page::FrameId;
    use std::time::{Duration, Instant};

    #[test]
    fn inflight_is_idempotent_on_redirect_and_unknown_done() {
        let now = Instant::now();
        let mut f = InFlight::new();
        f.on_request("a", false, now);
        f.on_request("a", true, now); // リダイレクト再送
        assert_eq!(f.len(), 1);
        f.on_done("zzz", now); // 未知ID
        assert_eq!(f.len(), 1);
        f.on_done("a", now);
        assert!(f.is_idle(now));
    }

    #[test]
    fn tombstone_ignores_late_request_until_expiry() {
        let now = Instant::now();
        let mut f = InFlight::new();
        f.on_done("x", now);
        f.on_request("x", false, now + Duration::from_millis(10));
        assert!(f.is_idle(now + Duration::from_millis(10)), "tombstone中の遅延挿入は無視される");
        f.on_request("x", false, now + Duration::from_millis(TOMBSTONE_MS + 1));
        assert!(!f.is_idle(now + Duration::from_millis(TOMBSTONE_MS + 1)), "失効後は通常どおり挿入される");
    }

    #[test]
    fn inflight_caps_tracked_ids() {
        let now = Instant::now();
        let mut f = InFlight::new();
        for i in 0..(MAX_TRACKED + 10) {
            f.on_request(&i.to_string(), false, now);
        }
        assert_eq!(f.len(), MAX_TRACKED);
    }

    #[test]
    fn decoded_budget_flags_first_exceed_and_stays() {
        let b = DecodedBudget::new(100);
        assert!(!b.on_data(60));
        assert!(b.on_data(50));
        assert!(b.on_data(1));
        assert!(b.exceeded());
        assert_eq!(b.total(), 111);
    }

    #[test]
    fn should_stop_matrix() {
        let cap = Duration::from_millis(5000);
        let t = Duration::from_millis(1000);
        assert!(should_stop(true, 2, 200, t, cap));
        assert!(!should_stop(false, 2, 200, t, cap));
        assert!(!should_stop(true, 1, 200, t, cap));
        assert!(!should_stop(true, 2, 199, t, cap));
        assert!(should_stop(false, 0, 0, cap, cap), "上限到達は他条件によらず停止");
    }

    #[test]
    fn main_navigation_requires_document_in_main_frame() {
        let main = FrameId::from("MAIN".to_string());
        let other = FrameId::from("SUB".to_string());
        assert!(is_main_navigation(&ResourceType::Document, &main, &main));
        assert!(!is_main_navigation(&ResourceType::Document, &other, &main));
        assert!(!is_main_navigation(&ResourceType::Image, &main, &main));
    }
}
```

- [ ] Step 2:失敗を確認

Run: `cargo test --lib render::wait`
Expected:コンパイルエラー（モジュール未定義）

- [ ] Step 3:実装

```rust
//! render経路の待機判定と計数（設計§4.2）。Chrome非依存の純関数群。

use chromiumoxide::cdp::browser_protocol::network::ResourceType;
use chromiumoxide::cdp::browser_protocol::page::FrameId;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

pub const POLL_MS: u64 = 250;
/// 「直前の観測との一致」がこの回数連続したら安定（観測はこの回数+1必要）。
pub const STABLE_POLLS: u32 = 2;
pub const TOMBSTONE_MS: u64 = 2000;
pub const MAX_TRACKED: usize = 4096;
/// 早期終了に必要な可視テキスト長。pipelineの短文閾値と同じ値。
pub const MIN_TEXT_CHARS: usize = 200;

/// 未完了要求の集合。挿入・削除とも冪等。削除済みIDは短命のtombstoneに残し、
/// 順序が入れ替わって後から届いた挿入を無視する。
pub struct InFlight {
    live: HashSet<String>,
    live_order: VecDeque<String>,
    tombstones: HashMap<String, Instant>,
}

impl Default for InFlight {
    fn default() -> Self {
        Self::new()
    }
}

impl InFlight {
    pub fn new() -> Self {
        Self { live: HashSet::new(), live_order: VecDeque::new(), tombstones: HashMap::new() }
    }

    pub fn on_request(&mut self, id: &str, _is_redirect: bool, now: Instant) {
        self.expire(now);
        if self.tombstones.contains_key(id) || self.live.contains(id) {
            return;
        }
        if self.live.len() >= MAX_TRACKED {
            if let Some(old) = self.live_order.pop_front() {
                self.live.remove(&old);
            }
        }
        self.live.insert(id.to_string());
        self.live_order.push_back(id.to_string());
    }

    pub fn on_done(&mut self, id: &str, now: Instant) {
        self.expire(now);
        self.live.remove(id);
        if self.tombstones.len() >= MAX_TRACKED {
            self.tombstones.clear();
        }
        self.tombstones.insert(id.to_string(), now);
    }

    pub fn is_idle(&mut self, now: Instant) -> bool {
        self.expire(now);
        self.live.is_empty()
    }

    pub fn len(&self) -> usize {
        self.live.len()
    }

    pub fn is_empty(&self) -> bool {
        self.live.is_empty()
    }

    fn expire(&mut self, now: Instant) {
        let ttl = Duration::from_millis(TOMBSTONE_MS);
        self.tombstones.retain(|_, t| now.duration_since(*t) < ttl);
        self.live_order.retain(|id| self.live.contains(id));
    }
}

/// 展開後バイトの累計と上限判定。監視タスクとポーリングが共有する。
pub struct DecodedBudget {
    max: u64,
    total: AtomicU64,
    exceeded: AtomicBool,
}

impl DecodedBudget {
    pub fn new(max: u64) -> Self {
        Self { max, total: AtomicU64::new(0), exceeded: AtomicBool::new(false) }
    }
    /// 加算し、上限超過ならtrue（以後もtrue）。
    pub fn on_data(&self, len: u64) -> bool {
        let t = self.total.fetch_add(len, Ordering::SeqCst) + len;
        if t > self.max {
            self.exceeded.store(true, Ordering::SeqCst);
        }
        self.exceeded()
    }
    pub fn exceeded(&self) -> bool {
        self.exceeded.load(Ordering::SeqCst)
    }
    pub fn total(&self) -> u64 {
        self.total.load(Ordering::SeqCst)
    }
}

/// 待機終了判定（設計§4.2 手順3・5）。
pub fn should_stop(idle: bool, stable_polls: u32, text_len: usize, elapsed: Duration, cap: Duration) -> bool {
    if elapsed >= cap {
        return true;
    }
    idle && stable_polls >= STABLE_POLLS && text_len >= MIN_TEXT_CHARS
}

/// メインナビゲーション（メインフレームのDocument要求）か（設計§4.2 手順4）。
pub fn is_main_navigation(resource_type: &ResourceType, frame_id: &FrameId, main_frame_id: &FrameId) -> bool {
    *resource_type == ResourceType::Document && frame_id == main_frame_id
}
```

`FrameId::from(String)`と`PartialEq`の有無は`chromiumoxide_cdp-0.9.1/src/cdp.rs`の`pub struct FrameId`定義（`#[derive(... PartialEq, Eq, Hash)]`と`impl From<String>`）で確認する。無ければ`.inner()`文字列比較にする。

- [ ] Step 4:確認

Run: `cargo test --lib render::wait`
Expected: PASS

- [ ] Step 5:コミット

```bash
git add src/render/wait.rs src/render.rs
git commit -m "feat(render): 待機判定と計数の純関数群 render/wait.rs を追加

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 7: renderproxy.rs — `HostCache`（2秒上限・共有）と遮断件数

Files:
- Modify: `src/renderproxy.rs`

Interfaces:
- Produces:
```rust
pub struct HostCache { .. }
impl HostCache {
    pub fn new(allow_private: bool) -> Self;
    /// 解決→netguard判定→検証済みIP。2秒上限、失敗・超過・内部はNone（fail-closed）。結果はキャッシュ。
    pub async fn resolve(&self, host: &str, port: u16) -> Option<SocketAddr>;
}
pub async fn spawn(cache: Arc<HostCache>, max_bytes: u64) -> io::Result<(SocketAddr, Arc<ProxyState>, JoinHandle<()>)>;
impl ProxyState { pub fn denied(&self) -> u64; }
```

- [ ] Step 1:失敗するテストを書く（既存`proxy_enforces_max_bytes_over_tunnel`等は`spawn(Arc::new(HostCache::new(true)), ...)`に変更）

```rust
    #[tokio::test]
    async fn host_cache_resolves_once_and_denies_internal() {
        let c = HostCache::new(false);
        assert!(c.resolve("127.0.0.1", 80).await.is_none(), "内部は遮断");
        assert!(c.resolve("nonexistent.invalid", 80).await.is_none(), "解決不能はfail-closed");
        let c2 = HostCache::new(true);
        let a = c2.resolve("127.0.0.1", 80).await;
        assert_eq!(a.map(|s| s.port()), Some(80));
        assert_eq!(c2.cached_len(), 1);
        let _ = c2.resolve("127.0.0.1", 80).await;
        assert_eq!(c2.cached_len(), 1, "2回目はキャッシュ");
    }

    #[tokio::test]
    async fn proxy_counts_denials() {
        let (addr, st, _h) = spawn(Arc::new(HostCache::new(false)), 1_000_000).await.unwrap();
        let mut c = TcpStream::connect(addr).await.unwrap();
        c.write_all(b"CONNECT 127.0.0.1:9 HTTP/1.1\r\nHost: 127.0.0.1:9\r\n\r\n").await.unwrap();
        let mut sink = Vec::new();
        let _ = c.read_to_end(&mut sink).await;
        assert!(String::from_utf8_lossy(&sink).starts_with("HTTP/1.1 403"));
        assert_eq!(st.denied(), 1);
    }
```

- [ ] Step 2:失敗を確認

Run: `cargo test --lib renderproxy`
Expected:コンパイルエラー

- [ ] Step 3:実装

`ProxyState`から`allow_private`を外し`cache: Arc<HostCache>`と`denied: AtomicU64`を持たせる。`validate_and_pin`を`HostCache::resolve`に置換し、`handle_connect` / `handle_http`の遮断分岐で`st.denied.fetch_add(1, Ordering::SeqCst)`する。

```rust
const RESOLVE_TIMEOUT: Duration = Duration::from_secs(2);

/// ホスト解決の実行単位キャッシュ。intercept層とプロキシ層で共有する（設計§4.2）。
pub struct HostCache {
    allow_private: bool,
    map: tokio::sync::Mutex<HashMap<(String, u16), Option<SocketAddr>>>,
}

impl HostCache {
    pub fn new(allow_private: bool) -> Self {
        Self { allow_private, map: tokio::sync::Mutex::new(HashMap::new()) }
    }

    pub async fn resolve(&self, host: &str, port: u16) -> Option<SocketAddr> {
        let key = (host.to_ascii_lowercase(), port);
        if let Some(v) = self.map.lock().await.get(&key) {
            return *v;
        }
        let h = host.to_string();
        let allow_private = self.allow_private;
        let task = tokio::task::spawn_blocking(move || {
            use std::net::ToSocketAddrs;
            let addrs: Vec<SocketAddr> = (h.as_str(), port).to_socket_addrs().ok()?.collect();
            if addrs.is_empty() {
                return None;
            }
            if !allow_private && addrs.iter().any(|a| netguard::is_internal(a.ip())) {
                return None;
            }
            Some(addrs[0])
        });
        // 上限超過はfail-closed（遮断）。spawn_blockingのスレッドは回収されないが実行単位で有界。
        let v = match tokio::time::timeout(RESOLVE_TIMEOUT, task).await {
            Ok(Ok(v)) => v,
            _ => None,
        };
        self.map.lock().await.insert(key, v);
        v
    }

    #[cfg(test)]
    pub fn cached_len(&self) -> usize {
        self.map.blocking_lock().len()
    }
}
```

`cached_len`は非同期テストから呼ぶので、`pub async fn cached_len(&self) -> usize { self.map.lock().await.len() }`と非同期関数にし、テスト側も`.await`する（`blocking_lock`は使わない）。

- [ ] Step 4:確認

Run: `cargo test --lib renderproxy`
Expected: PASS。`render.rs`の`renderproxy::spawn(opts.allow_private, ...)`呼び出しはコンパイルエラーになるのでTask 8で置換するまで`renderproxy::spawn(Arc::new(renderproxy::HostCache::new(opts.allow_private)), opts.max_bytes)`に暫定変更する。

- [ ] Step 5:コミット

```bash
git add src/renderproxy.rs src/render.rs
git commit -m "feat(renderproxy): ホスト解決キャッシュ(2秒上限)と遮断件数を追加

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 8: render.rs — §4.2の待機と単一経路の終了コード8

実装前に確定済みの事実（probeで実測。試行錯誤しないこと）を次に示す。

| 事実 | 実装への含意 |
|---|---|
| `page.mainframe()`は`about:blank`時点で`Some(FrameId)`を返し、`FrameId`は`PartialEq`と`.inner() -> &str`を持つ | 手順0でそのまま取得し、`wait::is_main_navigation`で`==`比較する |
| `about:blank`上で作った分離ワールドはナビゲーションで破棄され、古い`context_id`の評価は`Error -32000: Cannot find context with specified id`になる | `IsolatedWorld`は`goto`後に初めて`ensure_ctx`する（遅延生成）。評価が`Err`なら`ctx=None`にして1回だけ作り直す |
| 同じフレーム・同じ`world_name`で`Page.createIsolatedWorld`を再実行すると同じ`context_id`が返る | 作り直しは安全（多重生成しない） |
| `page.execute(EvaluateParams{context_id, return_by_value:true})`の戻りは`CommandResponse<EvaluateReturns>`で、値は`resp.result.result.value: Option<serde_json::Value>` | 数値配列は`as_array()`と`as_f64()`で読む |
| ページが`document.documentElement.outerHTML`のgetterを上書きすると、`page.content()`（メインワールド評価）は偽の3バイトを返すが、分離ワールドの評価は正しい長さを返す | DOM HTMLの取得は`page.content()`ではなく分離ワールドの評価（`IsolatedWorld::dom_html`）で行う |
| `tokio::sync::{Semaphore, Mutex}`は依存経由で有効 | Cargo.tomlは変更しない |
| `EventRequestPaused.network_id: Option<network::RequestId>`、`EventRequestWillBeSent.redirect_response: Option<_>`、`EventDataReceived.data_length: i64`、`RequestId::inner() -> &str` | in-flight集合のキーは`.inner()`の文字列 |
| `Fetch.failRequest`した要求にも`Network.loadingFailed`が届く。リダイレクトは同一IDで`requestWillBeSent`が再送され完了通知は1回。`data:`/`blob:`も`dataReceived`に計上される | 設計どおり集合方式で正しく空になる |
| メイン文書を`failRequest`すると`goto`は`Err("net::ERR_ACCESS_DENIED")`、iframeの遮断では`goto`は`Ok` | `goto`失敗経路で同期待ち後に`main_blocked`を見る |
| `BrowserConfig::builder().no_sandbox()`が存在する（`chromiumoxide-0.9.1/src/browser/config.rs:171`） | `--no-sandbox`はこれで実装する |


Files:
- Modify: `src/render.rs`（`drive`を全面置換、`render_inner`の後処理を変更）

Interfaces:
- Consumes: `wait::*`、`renderproxy::{HostCache, spawn, ProxyState}`
- Produces:
```rust
pub struct RenderOptions {
    pub timeout: Duration,        // render全体（deadline）
    pub wait_ms: u64,
    pub allow_private: bool,
    pub chrome_path: Option<String>,
    pub max_bytes: u64,           // 残余（展開後）
    pub max_bytes_total: u64,     // 利用者指定値（メッセージ用）
    pub no_sandbox: bool,
}
pub async fn render(url_str: &str, opts: &RenderOptions) -> Result<String>
```
  終了コード: 8（`main_blocked`、他の失敗より優先）、4（展開後超過・DOM長超過・プロキシ超過。messageは`render download exceeds remaining --max-bytes budget (N of M)`）、7（Chrome/CDP/タイムアウト/メインフレームID取得失敗）。stderrに`warn=netguard-blocked layer=<intercept|proxy> count=N`を遮断があったとき出す。

- [ ] Step 1:単体テスト（Chrome不要）を書く

```rust
    #[test]
    fn effective_cap_is_clamped_by_deadline() {
        let now = Instant::now();
        let deadline = now + Duration::from_millis(3500);
        assert_eq!(effective_cap(5000, deadline, now), Duration::from_millis(1500));
        assert_eq!(effective_cap(1000, deadline, now), Duration::from_millis(1000));
        assert_eq!(effective_cap(5000, now, now), Duration::ZERO);
    }

    #[test]
    fn exit8_takes_precedence_over_any_drive_result() {
        let blocked = Arc::new(AtomicBool::new(true));
        let r = finalize(&blocked, Err(WebgrabError::new(ExitCode::Http, "x")), 0, 0);
        assert_eq!(r.unwrap_err().code, ExitCode::Netguard);
        let r2 = finalize(&blocked, Ok("<html></html>".into()), 0, 0);
        assert_eq!(r2.unwrap_err().code, ExitCode::Netguard);
        let clear = Arc::new(AtomicBool::new(false));
        assert!(finalize(&clear, Ok("<html></html>".into()), 0, 0).is_ok());
    }
```

- [ ] Step 2:失敗を確認

Run: `cargo test --lib render::tests`
Expected:コンパイルエラー（`effective_cap` / `finalize`未定義）

- [ ] Step 3:実装

`render.rs`の本体を次に置換する（`proxy_args`と既存テストは維持。`host_is_internal`は`HostCache`を使う形に変更）。

```rust
//! JSレンダリング（設計 08 §4.2）。chromiumoxide + CDP Fetch interception + Network監視。
//!
//! SSRFは二層で防ぐ。第一層はCDP Fetchドメインでページセッションの全リクエストを横取りし、
//! 宛先ホストをnetguardで判定して内部アドレス宛を遮断する（fail-closed）。第二層は
//! [`renderproxy`]の検証・IPピン留めプロキシで、Chromeの全接続（OOPIF/Service Workerを含む）を
//! 経由させ、判定と接続のIP一致を保証してDNSリバインディング(TOCTOU)を閉じる。
//! `--max-bytes`は`Network.dataReceived`の展開後バイト（ページセッション）と、
//! `content()`前のDOM長評価で有界にする。

pub mod wait;

use crate::error::{ExitCode, Result, WebgrabError};
use crate::netguard;
use crate::renderproxy::{self, HostCache, ProxyState};
use chromiumoxide::browser::{Browser, BrowserConfig};
use chromiumoxide::cdp::browser_protocol::fetch::{
    ContinueRequestParams, EnableParams, EventRequestPaused, FailRequestParams,
};
use chromiumoxide::cdp::browser_protocol::network::{
    ErrorReason, EventDataReceived, EventLoadingFailed, EventLoadingFinished,
    EventRequestWillBeSent,
};
use chromiumoxide::cdp::browser_protocol::page::{CreateIsolatedWorldParams, FrameId};
use chromiumoxide::cdp::js_protocol::runtime::{EvaluateParams, ExecutionContextId};
use chromiumoxide::page::Page;
use futures::StreamExt;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use url::Url;
use wait::{DecodedBudget, InFlight};

pub struct RenderOptions {
    pub timeout: Duration,
    pub wait_ms: u64,
    pub allow_private: bool,
    pub chrome_path: Option<String>,
    /// 残余の展開後バイト上限（超過は終了コード4）。
    pub max_bytes: u64,
    /// 利用者指定の--max-bytes（メッセージ表示用）。
    pub max_bytes_total: u64,
    pub no_sandbox: bool,
}

const CONTENT_RESERVE: Duration = Duration::from_millis(2000);
const NAV_WAIT_MAX: Duration = Duration::from_millis(1000);
const SYNC_WAIT_MAX: Duration = Duration::from_millis(500);
const INTERCEPT_CONCURRENCY: usize = 16;

/// Dropでabortするタスクガード。早期リターン・--timeoutキャンセルでもタスクを残置しない。
struct AbortOnDrop(tokio::task::JoinHandle<()>);
impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// drive/監視/interceptが共有する状態。
struct Shared {
    main_blocked: AtomicBool,
    inflight: Mutex<InFlight>,
    decoded: DecodedBudget,
    blocked_intercept: AtomicU64,
    received: AtomicU64,
    processed: AtomicU64,
    cache: Arc<HostCache>,
    allow_private: bool,
    main_frame: FrameId,
}

fn proxy_args(port: u16) -> [String; 2] { /* 既存のまま */ }

/// `goto`直前に確定する実効待機上限: min(--wait-ms, deadline − now − 予備2000ms)。
fn effective_cap(wait_ms: u64, deadline: Instant, now: Instant) -> Duration {
    let remaining = deadline.saturating_duration_since(now).saturating_sub(CONTENT_RESERVE);
    Duration::from_millis(wait_ms).min(remaining)
}

/// 終了コード8を単一経路で判定する（設計§4.2 手順7）。driveの結果によらず先にmain_blockedを見る。
fn finalize(
    main_blocked: &AtomicBool,
    result: Result<String>,
    blocked_intercept: u64,
    blocked_proxy: u64,
) -> Result<String> {
    if main_blocked.load(Ordering::SeqCst) {
        return Err(WebgrabError::new(ExitCode::Netguard, "refused internal address during render")
            .with_detail(format!("layer=intercept main-navigation blocked (intercept={blocked_intercept} proxy={blocked_proxy})")));
    }
    if blocked_intercept > 0 {
        eprintln!("webgrab: warn=netguard-blocked layer=intercept count={blocked_intercept}");
    }
    if blocked_proxy > 0 {
        eprintln!("webgrab: warn=netguard-blocked layer=proxy count={blocked_proxy}");
    }
    result
}

pub async fn render(url_str: &str, opts: &RenderOptions) -> Result<String> {
    let deadline = Instant::now() + opts.timeout;
    match tokio::time::timeout(opts.timeout, render_inner(url_str, opts, deadline)).await {
        Ok(r) => r,
        Err(_) => Err(WebgrabError::new(ExitCode::Render, "render timed out (--timeout exceeded)")),
    }
}

async fn render_inner(url_str: &str, opts: &RenderOptions, deadline: Instant) -> Result<String> {
    let user_data = tempfile::Builder::new().prefix("webgrab-chrome-").tempdir().map_err(|e| {
        WebgrabError::new(ExitCode::Render, "temp dir failed").with_detail(e.to_string())
    })?;
    let cache = Arc::new(HostCache::new(opts.allow_private));
    let (proxy_addr, proxy_state, proxy_handle) = renderproxy::spawn(cache.clone(), opts.max_bytes)
        .await
        .map_err(|e| WebgrabError::new(ExitCode::Render, "ssrf proxy start failed").with_detail(e.to_string()))?;
    let _proxy_guard = AbortOnDrop(proxy_handle);

    let mut builder = BrowserConfig::builder()
        .new_headless_mode()
        .user_data_dir(user_data.path())
        .args(proxy_args(proxy_addr.port()));
    if opts.no_sandbox {
        builder = builder.no_sandbox();
    }
    if let Some(p) = &opts.chrome_path {
        builder = builder.chrome_executable(p);
    }
    let config = builder
        .build()
        .map_err(|e| WebgrabError::new(ExitCode::Render, "chrome config failed").with_detail(e))?;
    let (mut browser, mut handler) = Browser::launch(config).await.map_err(|e| {
        WebgrabError::new(ExitCode::Render, "chrome launch failed (is Chrome installed?)").with_detail(e.to_string())
    })?;
    let handler_task = tokio::spawn(async move { while handler.next().await.is_some() {} });

    let main_blocked = Arc::new(AtomicBool::new(false));
    let (result, blocked_intercept) = drive(&mut browser, url_str, opts, deadline, cache, &proxy_state, main_blocked.clone()).await;

    let _ = browser.close().await;
    let _ = handler_task.await;

    finalize(&main_blocked, result, blocked_intercept, proxy_state.denied())
}

async fn drive(
    browser: &mut Browser,
    url_str: &str,
    opts: &RenderOptions,
    deadline: Instant,
    cache: Arc<HostCache>,
    proxy_state: &ProxyState,
    main_blocked: Arc<AtomicBool>,
) -> (Result<String>, u64) {
    let shared_holder: Arc<Mutex<Option<Arc<Shared>>>> = Arc::new(Mutex::new(None));
    let r = drive_inner(browser, url_str, opts, deadline, cache, proxy_state, main_blocked, shared_holder.clone()).await;
    let blocked = shared_holder.lock().unwrap().as_ref().map(|s| s.blocked_intercept.load(Ordering::SeqCst)).unwrap_or(0);
    (r, blocked)
}

async fn drive_inner(
    browser: &mut Browser,
    url_str: &str,
    opts: &RenderOptions,
    deadline: Instant,
    cache: Arc<HostCache>,
    proxy_state: &ProxyState,
    main_blocked: Arc<AtomicBool>,
    shared_holder: Arc<Mutex<Option<Arc<Shared>>>>,
) -> Result<String> {
    let render_err = |m: &'static str, e: String| WebgrabError::new(ExitCode::Render, m).with_detail(e);
    let page = browser.new_page("about:blank").await.map_err(|e| render_err("new page failed", e.to_string()))?;

    // 手順0: メインフレームID（Fetch.enable前に取得。取れなければfail-closedで終了コード7）
    let main_frame = page
        .mainframe()
        .await
        .map_err(|e| render_err("main frame id unavailable", e.to_string()))?
        .ok_or_else(|| WebgrabError::new(ExitCode::Render, "main frame id unavailable"))?;

    let shared = Arc::new(Shared {
        main_blocked: AtomicBool::new(false),
        inflight: Mutex::new(InFlight::new()),
        decoded: DecodedBudget::new(opts.max_bytes),
        blocked_intercept: AtomicU64::new(0),
        received: AtomicU64::new(0),
        processed: AtomicU64::new(0),
        cache,
        allow_private: opts.allow_private,
        main_frame: main_frame.clone(),
    });
    *shared_holder.lock().unwrap() = Some(shared.clone());
    // main_blockedは外側(finalize)が読むArcへ転写するため、Shared側の変化を都度反映する。
    let mirror = main_blocked;

    page.execute(EnableParams::default()).await.map_err(|e| render_err("fetch enable failed", e.to_string()))?;

    // 手順1: 監視タスク（Network 4イベント）
    let mut sent = page.event_listener::<EventRequestWillBeSent>().await.map_err(|e| render_err("listener failed", e.to_string()))?;
    let mut fin = page.event_listener::<EventLoadingFinished>().await.map_err(|e| render_err("listener failed", e.to_string()))?;
    let mut fail = page.event_listener::<EventLoadingFailed>().await.map_err(|e| render_err("listener failed", e.to_string()))?;
    let mut data = page.event_listener::<EventDataReceived>().await.map_err(|e| render_err("listener failed", e.to_string()))?;
    let sh = shared.clone();
    let _monitor = AbortOnDrop(tokio::spawn(async move {
        loop {
            tokio::select! {
                Some(ev) = sent.next() => {
                    sh.inflight.lock().unwrap().on_request(ev.request_id.inner(), ev.redirect_response.is_some(), Instant::now());
                }
                Some(ev) = fin.next() => { sh.inflight.lock().unwrap().on_done(ev.request_id.inner(), Instant::now()); }
                Some(ev) = fail.next() => { sh.inflight.lock().unwrap().on_done(ev.request_id.inner(), Instant::now()); }
                Some(ev) = data.next() => { sh.decoded.on_data(ev.data_length.max(0) as u64); }
                else => break,
            }
        }
    }));

    // 手順1: interceptタスク（個別タスク化、同時16、ホスト判定キャッシュ）
    let mut paused = page.event_listener::<EventRequestPaused>().await.map_err(|e| render_err("listener failed", e.to_string()))?;
    let page_i = page.clone();
    let sh = shared.clone();
    let mirror_i = mirror.clone();
    let _intercept = AbortOnDrop(tokio::spawn(async move {
        let sem = Arc::new(tokio::sync::Semaphore::new(INTERCEPT_CONCURRENCY));
        while let Some(ev) = paused.next().await {
            sh.received.fetch_add(1, Ordering::SeqCst);
            let permit = sem.clone().acquire_owned().await;
            let (page, sh, mirror) = (page_i.clone(), sh.clone(), mirror_i.clone());
            tokio::spawn(async move {
                let _permit = permit;
                let deny = host_is_internal(&sh.cache, &ev.request.url, sh.allow_private).await;
                if deny {
                    sh.blocked_intercept.fetch_add(1, Ordering::SeqCst);
                    if wait::is_main_navigation(&ev.resource_type, &ev.frame_id, &sh.main_frame) {
                        sh.main_blocked.store(true, Ordering::SeqCst);
                        mirror.store(true, Ordering::SeqCst);
                    }
                    if let Some(nid) = &ev.network_id {
                        sh.inflight.lock().unwrap().on_done(nid.inner(), Instant::now());
                    }
                    if let Ok(p) = FailRequestParams::builder().request_id(ev.request_id.clone()).error_reason(ErrorReason::AccessDenied).build() {
                        let _ = page.execute(p).await;
                    }
                } else if let Ok(p) = ContinueRequestParams::builder().request_id(ev.request_id.clone()).build() {
                    let _ = page.execute(p).await;
                }
                sh.processed.fetch_add(1, Ordering::SeqCst);
            });
        }
    }));

    let blocked_now = |sh: &Shared| sh.main_blocked.load(Ordering::SeqCst);
    let netguard_err = || WebgrabError::new(ExitCode::Netguard, "refused internal address during render");
    let exceed_err = |sh: &Shared, what: &str| {
        WebgrabError::new(ExitCode::Http, format!(
            "render download exceeds remaining --max-bytes budget ({} of {}) [{what}]",
            sh.decoded.total().max(opts.max_bytes), opts.max_bytes_total
        ))
    };

    // 手順2: goto（失敗時も同期待ち+再確認してから8/7を決める）
    let t_goto = Instant::now();
    let cap = effective_cap(opts.wait_ms, deadline, t_goto);
    if let Err(e) = page.goto(url_str).await {
        sync_wait(&shared).await;
        if blocked_now(&shared) { return Err(netguard_err()); }
        return Err(render_err("navigation failed", e.to_string()));
    }
    let nav_wait = NAV_WAIT_MAX.min(deadline.saturating_duration_since(Instant::now()));
    let _ = tokio::time::timeout(nav_wait, page.wait_for_navigation()).await; // 最善努力

    // 分離ワールド（ページ側の上書きが効かない文脈で評価する）。goto後に遅延生成する（about:blankの文脈は破棄済み）。
    let mut world = IsolatedWorld { page: page.clone(), frame: main_frame.clone(), ctx: None };

    // 手順3〜5: ポーリング
    let mut prev: Option<[u64; 2]> = None;
    let mut stable: u32 = 0;
    loop {
        if blocked_now(&shared) { return Err(netguard_err()); }
        if shared.decoded.exceeded() { return Err(exceed_err(&shared, "network")); }
        let idle = shared.inflight.lock().unwrap().is_idle(Instant::now());
        let remaining = deadline.saturating_duration_since(Instant::now());
        match world.measure(remaining).await {
            Some(cur) => {
                if prev == Some(cur) { stable += 1 } else { stable = 0 }
                prev = Some(cur);
            }
            None => stable = 0,
        }
        let text_len = prev.map(|c| c[1] as usize).unwrap_or(0);
        if wait::should_stop(idle, stable, text_len, t_goto.elapsed(), cap) { break; }
        let left = cap.saturating_sub(t_goto.elapsed());
        tokio::time::sleep(Duration::from_millis(wait::POLL_MS).min(left)).await;
    }

    // 手順6: 同期待ち → 再確認 → DOM長 → content()
    sync_wait(&shared).await;
    if blocked_now(&shared) { return Err(netguard_err()); }
    if shared.decoded.exceeded() { return Err(exceed_err(&shared, "network")); }
    let remaining = deadline.saturating_duration_since(Instant::now());
    if let Some(dom_len) = world.dom_length(remaining).await {
        let budget_left = opts.max_bytes.saturating_sub(shared.decoded.total());
        if dom_len > budget_left { return Err(exceed_err(&shared, "dom")); }
    }
    let remaining = deadline.saturating_duration_since(Instant::now());
    // page.content()はメインワールド評価でgetter上書きに弱いため、分離ワールドで取得する。
    let content = world
        .dom_html(remaining)
        .await
        .ok_or_else(|| WebgrabError::new(ExitCode::Render, "content read failed or timed out"))?;
    if blocked_now(&shared) { return Err(netguard_err()); }
    let _ = proxy_state; // 超過はfinalize後にrender_innerが見る（denied()のみ使用）
    Ok(content)
}

/// interceptが受け取ったイベントの処理完了を最大500ms待つ（設計§4.2 手順6）。
async fn sync_wait(shared: &Shared) {
    let start = Instant::now();
    while start.elapsed() < SYNC_WAIT_MAX {
        if shared.processed.load(Ordering::SeqCst) >= shared.received.load(Ordering::SeqCst) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// 分離ワールドでの数値評価。ページ側のdefineProperty等の上書きが効かない。
struct IsolatedWorld {
    page: Page,
    frame: FrameId,
    ctx: Option<ExecutionContextId>,
}

impl IsolatedWorld {
    async fn ensure_ctx(&mut self) -> Option<ExecutionContextId> {
        if let Some(c) = &self.ctx { return Some(c.clone()); }
        let r = self.page.execute(
            CreateIsolatedWorldParams::builder().frame_id(self.frame.clone()).world_name("webgrab").build().ok()?
        ).await.ok()?;
        self.ctx = Some(r.execution_context_id.clone());
        self.ctx.clone()
    }

    /// 式を評価してu64配列で返す。失敗・タイムアウト・非数値はNone（条件未達扱い）。
    async fn eval_numbers(&mut self, expr: &str, limit: Duration) -> Option<Vec<u64>> {
        for attempt in 0..2 {
            let ctx = self.ensure_ctx().await?;
            let params = EvaluateParams::builder().expression(expr).context_id(ctx).return_by_value(true).build().ok()?;
            match tokio::time::timeout(limit, self.page.execute(params)).await {
                Ok(Ok(resp)) => {
                    let v = resp.result.result.value.clone()?;
                    let arr = v.as_array()?;
                    return arr.iter().map(|x| x.as_f64().map(|f| f.max(0.0) as u64)).collect();
                }
                Ok(Err(_)) if attempt == 0 => { self.ctx = None; continue; } // 文脈破棄→作り直して1回だけ再試行
                _ => return None,
            }
        }
        None
    }

    async fn measure(&mut self, limit: Duration) -> Option<[u64; 2]> {
        let v = self.eval_numbers(
            "(function(){var b=document.body;return [document.getElementsByTagName('*').length,(b&&b.innerText||'').trim().length];})()",
            limit,
        ).await?;
        Some([*v.first()?, *v.get(1)?])
    }

    async fn dom_length(&mut self, limit: Duration) -> Option<u64> {
        let v = self.eval_numbers("(function(){var d=document.documentElement;return [d?d.outerHTML.length:0];})()", limit).await?;
        v.first().copied()
    }

    /// DOM HTML（doctype + outerHTML）を分離ワールドで取得する。失敗・タイムアウトはNone。
    async fn dom_html(&mut self, limit: Duration) -> Option<String> {
        const EXPR: &str = "(function(){var s='';if(document.doctype){s=new XMLSerializer().serializeToString(document.doctype);}var d=document.documentElement;if(d){s+=d.outerHTML;}return s;})()";
        for attempt in 0..2 {
            let ctx = self.ensure_ctx().await?;
            let params = EvaluateParams::builder().expression(EXPR).context_id(ctx).return_by_value(true).build().ok()?;
            match tokio::time::timeout(limit, self.page.execute(params)).await {
                Ok(Ok(resp)) => return resp.result.result.value.as_ref()?.as_str().map(|s| s.to_string()),
                Ok(Err(_)) if attempt == 0 => { self.ctx = None; continue; }
                _ => return None,
            }
        }
        None
    }
}

/// リクエストURLのホストを判定する（第一層）。http(s)以外はChromeに任せる。
async fn host_is_internal(cache: &HostCache, request_url: &str, allow_private: bool) -> bool {
    if allow_private { return false; }
    let Ok(u) = Url::parse(request_url) else { return false; };
    if !netguard::is_allowed_scheme(u.scheme()) { return false; }
    let Some(host) = u.host_str() else { return false; };
    let port = u.port_or_known_default().unwrap_or(80);
    cache.resolve(host, port).await.is_none()
}
```

`Shared.main_blocked`と外側の`mirror`の二重管理は、`finalize`が`drive`の戻り値と独立に読めるArcを持つため。`EvaluateParams`の`context_id` / `return_by_value`ビルダ、`CreateIsolatedWorldReturns.execution_context_id`、`EventRequestWillBeSent.redirect_response`、`EventDataReceived.data_length: i64`、`RequestId::inner()`は`chromiumoxide_cdp-0.9.1/src/cdp.rs`で確認済み。`response.result.result.value`の型は`CommandResponse<EvaluateReturns>` → `EvaluateReturns.result: RemoteObject` → `value: Option<serde_json::Value>`。

既存テスト`allow_private_short_circuits`等は`host_is_internal(&HostCache::new(false), "...", false)`の形に更新する。

- [ ] Step 4:確認

Run: `cargo test --lib render && cargo clippy --all-targets -- -D warnings`
Expected: PASS /警告なし。`pipeline.rs`の`RenderOptions`構築に`max_bytes_total: cli.max_bytes, no_sandbox: cli.no_sandbox`を暫定追加する。

- [ ] Step 5:実機スモーク（ローカル、Chromeあり）。上の確定事項の表と食い違う挙動が出た場合だけ、`examples/`に使い捨てprobeを書いて事実を確認し、設計書§1に追記してから直す（推測で直さない）

Run: `cargo run -- https://example.com --render --format json --max-chars 80 | python3 -c "import sys,json;d=json.load(sys.stdin);print(d['render_status'],d['total_chars'])"`
Expected: `rendered <数百>`。終了コード0。

- [ ] Step 6:コミット

```bash
git add src/render.rs
git commit -m "feat(render): ネットワーク静止+DOM安定待機、展開後バイト上限、終了コード8の単一判定

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 9: fetch.rs — 消費バイト数

Files:
- Modify: `src/fetch.rs:20-24`, `:266-271`

Interfaces:
- Produces: `Fetched.consumed_bytes: u64`（最終応答の展開後本文長。リダイレクト中間応答とrobots.txtは含めない）

- [ ] Step 1:既存統合テストが壊れないことを前提に実装（フィールド追加のみ）

```rust
pub struct Fetched {
    pub final_url: String,
    pub content_type: Option<String>,
    pub body: Vec<u8>,
    /// 最終応答の展開後バイト数（残余--max-bytesの計算に使う）。
    pub consumed_bytes: u64,
}
```
`Ok(Fetched { final_url: current.to_string(), content_type, consumed_bytes: body.len() as u64, body })`

- [ ] Step 2:確認

Run: `cargo test`
Expected:緑

- [ ] Step 3:コミット

```bash
git add src/fetch.rs
git commit -m "feat(fetch): Fetchedに展開後の消費バイト数を追加

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 10: pipeline.rs — 3フェーズ再構成と純関数

Files:
- Modify: `src/pipeline.rs`（全面置換）
- Modify: `tests/integration.rs`（F1回帰、`flag-ignored`、skip契約）

Interfaces:
- Consumes: Task 1〜9の全産物
- Produces:
```rust
pub enum Phase { Render, Extract }
pub enum SkipReason { Timeout, MaxBytes }   // token(): "timeout" | "max-bytes"
pub fn escalation_reason(visible_chars: usize) -> Option<&'static str>          // 0→"empty", <200→"short", else None
pub fn remaining_budget(timeout: Duration, elapsed: Duration, max_bytes: u64, consumed: u64) -> std::result::Result<(Duration, u64), SkipReason>
pub fn choose_result(static_chars: usize, rendered_chars: usize) -> RenderStatus   // rendered > static → Rendered else NoGain
pub fn fallback_reason(phase: Phase, err: &WebgrabError) -> Option<&'static str>
pub fn hint_for(status: RenderStatus) -> (&'static str, &'static str)             // (stderr token, prose)
```

- [ ] Step 1:単体テストを書く（`src/pipeline.rs`のtests）

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escalation_reason_thresholds() {
        assert_eq!(escalation_reason(0), Some("empty"));
        assert_eq!(escalation_reason(199), Some("short"));
        assert_eq!(escalation_reason(200), None);
    }

    #[test]
    fn remaining_budget_skips_below_thresholds() {
        let t = Duration::from_secs(30);
        assert!(matches!(remaining_budget(t, Duration::from_secs(26), 20 << 20, 0), Err(SkipReason::Timeout)));
        assert!(matches!(remaining_budget(t, Duration::from_secs(1), 300 * 1024, 100 * 1024), Err(SkipReason::MaxBytes)));
        let (rt, rb) = remaining_budget(t, Duration::from_secs(10), 20 << 20, 1 << 20).unwrap();
        assert_eq!(rt, Duration::from_secs(20));
        assert_eq!(rb, (20 << 20) - (1 << 20));
        assert!(matches!(remaining_budget(t, Duration::from_secs(40), 20 << 20, 0), Err(SkipReason::Timeout)), "経過が予算超過なら0扱い");
    }

    #[test]
    fn choose_result_prefers_longer() {
        assert_eq!(choose_result(150, 400), RenderStatus::Rendered);
        assert_eq!(choose_result(150, 150), RenderStatus::NoGain);
        assert_eq!(choose_result(150, 20), RenderStatus::NoGain);
    }

    #[test]
    fn fallback_reason_by_phase() {
        let e8 = WebgrabError::new(ExitCode::Netguard, "x");
        let e7 = WebgrabError::new(ExitCode::Render, "x");
        let e4 = WebgrabError::new(ExitCode::Http, "x");
        let e1 = WebgrabError::new(ExitCode::Internal, "x");
        assert_eq!(fallback_reason(Phase::Render, &e8), None);
        assert_eq!(fallback_reason(Phase::Render, &e7), Some("render"));
        assert_eq!(fallback_reason(Phase::Render, &e4), Some("max-bytes"));
        assert_eq!(fallback_reason(Phase::Render, &e1), None);
        assert_eq!(fallback_reason(Phase::Extract, &e4), Some("extract"));
        assert_eq!(fallback_reason(Phase::Extract, &e1), Some("extract"));
    }

    #[test]
    fn hint_follows_render_status() {
        assert_eq!(hint_for(RenderStatus::Static), ("--render/--raw", "--render or --raw"));
        assert_eq!(hint_for(RenderStatus::Skipped("timeout")), ("--render/--raw", "--render or --raw"));
        assert_eq!(hint_for(RenderStatus::Rendered), ("--raw", "--raw"));
        assert_eq!(hint_for(RenderStatus::Failed("render")), ("--raw", "--raw"));
        assert_eq!(hint_for(RenderStatus::NoGain), ("--raw", "--raw"));
    }
}
```

統合テスト（`tests/integration.rs`に追加。`spawn_server`は既存のものを使う）を次に示す。

```rust
const EMPTY_SHELL: &str = "<html><head><title>CSR</title></head><body><div id=\"app\"></div><script>setTimeout(function(){document.getElementById('app').innerHTML='<p>late</p>'},500)</script></body></html>";

#[test]
fn empty_shell_exits_6_with_hint_token() {
    let port = spawn_server(3, |path| {
        if path == "/robots.txt" {
            return "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into();
        }
        http_response(EMPTY_SHELL, "text/html; charset=utf-8")
    });
    let url = format!("http://127.0.0.1:{port}/shell");
    let (code, _stdout, stderr) = run_webgrab(&[&url, "--allow-private"]);
    assert_eq!(code, 6, "stderr={stderr}");
    assert!(stderr.lines().any(|l| l.starts_with("webgrab: error=empty hint=--render/--raw")), "{stderr}");
    assert!(stderr.contains("warn=extract-grab-failed"));
}

#[test]
fn wait_ms_without_render_is_ignored_with_warning() {
    let port = spawn_server(3, |path| {
        if path == "/robots.txt" {
            return "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into();
        }
        http_response(ARTICLE, "text/html; charset=utf-8")
    });
    let url = format!("http://127.0.0.1:{port}/a");
    let (code, _stdout, stderr) = run_webgrab(&[&url, "--allow-private", "--wait-ms", "100"]);
    assert_eq!(code, 0);
    assert!(stderr.contains("warn=flag-ignored flag=--wait-ms"), "{stderr}");
}

const SHORT_ARTICLE: &str = "<html><head><title>短い</title></head><body><article><p>これは百五十文字程度の短い本文です。抽出器が本文として認識できる長さはありますが、二百文字には届きません。エスカレーション判定の境界を確認するための固定文です。末尾。</p></article></body></html>";

#[test]
fn auto_render_skips_when_budget_is_short() {
    // --timeout 3 なら残余は常に5秒未満 → skip。Chrome不要。
    let port = spawn_server(3, |path| {
        if path == "/robots.txt" {
            return "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into();
        }
        http_response(SHORT_ARTICLE, "text/html; charset=utf-8")
    });
    let url = format!("http://127.0.0.1:{port}/short");
    let (code, stdout, stderr) = run_webgrab(&[&url, "--allow-private", "--auto-render", "--timeout", "3"]);
    assert_eq!(code, 0, "stderr={stderr}");
    assert!(stderr.contains("warn=auto-render-skipped reason=timeout"), "{stderr}");
    let i_short = stdout.find("[webgrab:short-content").expect("short-content");
    let i_rs = stdout.find("[webgrab:render-status skipped reason=timeout]").expect("render-status");
    assert!(i_short < i_rs, "{stdout}");
    assert!(stdout.contains("retry with --render or --raw]"), "{stdout}");
}
```

- [ ] Step 2:失敗を確認

Run: `cargo test --lib pipeline && cargo test --test integration`
Expected:コンパイルエラー/統合3件失敗

- [ ] Step 3:実装（`src/pipeline.rs`全面置換）

```rust
//! パイプライン結線（設計 08 §4.3）。静的フェーズ → エスカレーション判定 → renderフェーズ → 出力。

use crate::cli::{Cli, DEFAULT_WAIT_MS, FormatArg};
use crate::error::{ExitCode, Result, WebgrabError};
use crate::fetch::{self, FetchOptions};
use crate::output::{self, Format, Meta, RenderStatus};
use crate::render::{self, RenderOptions};
use crate::{budget, cli, convert, decode, extract, tokens};
use std::time::{Duration, Instant};

const SHORT_CONTENT_CHARS: usize = 200;
const SKIP_TIMEOUT_MIN: Duration = Duration::from_secs(5);
const SKIP_BYTES_MIN: u64 = 256 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase { Render, Extract }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason { Timeout, MaxBytes }

impl SkipReason {
    pub fn token(self) -> &'static str {
        match self { SkipReason::Timeout => "timeout", SkipReason::MaxBytes => "max-bytes" }
    }
}

pub fn escalation_reason(visible_chars: usize) -> Option<&'static str> {
    if visible_chars == 0 { Some("empty") } else if visible_chars < SHORT_CONTENT_CHARS { Some("short") } else { None }
}

pub fn remaining_budget(timeout: Duration, elapsed: Duration, max_bytes: u64, consumed: u64) -> std::result::Result<(Duration, u64), SkipReason> {
    let rt = timeout.saturating_sub(elapsed);
    if rt < SKIP_TIMEOUT_MIN { return Err(SkipReason::Timeout); }
    let rb = max_bytes.saturating_sub(consumed);
    if rb < SKIP_BYTES_MIN { return Err(SkipReason::MaxBytes); }
    Ok((rt, rb))
}

pub fn choose_result(static_chars: usize, rendered_chars: usize) -> RenderStatus {
    if rendered_chars > static_chars { RenderStatus::Rendered } else { RenderStatus::NoGain }
}

pub fn fallback_reason(phase: Phase, err: &WebgrabError) -> Option<&'static str> {
    match phase {
        Phase::Render => match err.code {
            ExitCode::Render => Some("render"),
            ExitCode::Http => Some("max-bytes"),
            _ => None,
        },
        Phase::Extract => Some("extract"),
    }
}

/// (stderrトークン, 本文用散文)。設計§4.1: static/skippedは両方提案、renderしたなら--rawのみ。
pub fn hint_for(status: RenderStatus) -> (&'static str, &'static str) {
    match status {
        RenderStatus::Static | RenderStatus::Skipped(_) => ("--render/--raw", "--render or --raw"),
        RenderStatus::Rendered | RenderStatus::Failed(_) | RenderStatus::NoGain => ("--raw", "--raw"),
    }
}

fn to_format(f: FormatArg) -> Format {
    match f {
        FormatArg::Markdown => Format::Markdown,
        FormatArg::Frontmatter => Format::Frontmatter,
        FormatArg::Json => Format::Json,
        FormatArg::Text => Format::Text,
        FormatArg::Html => Format::Html,
    }
}

/// 抽出・変換済みの中間結果。
struct Stage {
    title: Option<String>,
    published: Option<String>,
    body: String,
    visible: usize,
}

fn build_stage(cli: &Cli, html: &str, final_url: &str) -> Result<Stage> {
    let (title, published, body_html) = if cli.raw {
        (None, None, convert::strip_non_content(html))
    } else {
        let ex = extract::extract(html, final_url)?;
        (ex.title, ex.published_time, ex.content_html)
    };
    let body = match to_format(cli.format) {
        Format::Html => body_html.clone(),
        Format::Text => convert::to_text(&body_html)?,
        _ => convert::to_markdown(&body_html)?,
    };
    let visible = convert::visible_text_len(&body_html);
    Ok(Stage { title, published, body, visible })
}

fn render_options(cli: &Cli, timeout: Duration, max_bytes: u64) -> RenderOptions {
    RenderOptions {
        timeout,
        wait_ms: cli.wait_ms.unwrap_or(DEFAULT_WAIT_MS),
        allow_private: cli.allow_private,
        chrome_path: cli.chrome_path.clone(),
        max_bytes,
        max_bytes_total: cli.max_bytes,
        no_sandbox: cli.no_sandbox,
    }
}

/// CLIを実行し、最終出力文字列を返す。
pub async fn run(cli: &Cli) -> Result<String> {
    let start = Instant::now();
    let ua = cli.user_agent.clone().unwrap_or_else(cli::default_user_agent);
    let timeout = Duration::from_secs(cli.timeout);
    if cli.wait_ms.is_some() && !cli.render && !cli.auto_render {
        eprintln!("webgrab: warn=flag-ignored flag=--wait-ms");
    }
    if cli.no_sandbox && (cli.render || cli.auto_render) {
        eprintln!("webgrab: warn=no-sandbox");
    }

    // 1. 静的フェーズ（または --render 明示）
    let mut status = RenderStatus::Static;
    let mut static_chars: Option<usize> = None;
    let mut rendered_chars: Option<usize> = None;
    let (html, final_url, consumed) = if cli.render {
        if !cli.no_robots {
            let fopts = FetchOptions { user_agent: ua, timeout, max_bytes: cli.max_bytes, allow_private: cli.allow_private, check_robots: true };
            if !fetch::robots_precheck(&cli.url, &fopts).await? {
                return Err(WebgrabError::new(ExitCode::Robots, "blocked by robots.txt").with_detail(format!("url={}", cli.url)));
            }
        }
        let dom = render::render(&cli.url, &render_options(cli, timeout, cli.max_bytes)).await?;
        status = RenderStatus::Rendered;
        (dom, cli.url.clone(), 0u64)
    } else {
        let fopts = FetchOptions { user_agent: ua, timeout, max_bytes: cli.max_bytes, allow_private: cli.allow_private, check_robots: !cli.no_robots };
        let fetched = fetch::fetch(&cli.url, &fopts).await?;
        let (text, enc, had_errors) = decode::decode(&fetched.body, fetched.content_type.as_deref());
        if had_errors {
            eprintln!("webgrab: warn=decode-replacement enc={enc}");
        }
        (text, fetched.final_url, fetched.consumed_bytes)
    };

    let mut stage = build_stage(cli, &html, &final_url)?;
    if cli.render {
        rendered_chars = Some(stage.visible);
    } else {
        static_chars = Some(stage.visible);
    }

    // 2〜5. エスカレーション（--auto-render、--render明示時は無効）
    if cli.auto_render && !cli.render
        && let Some(reason) = escalation_reason(stage.visible)
    {
        match remaining_budget(timeout, start.elapsed(), cli.max_bytes, consumed) {
            Err(skip) => {
                eprintln!("webgrab: warn=auto-render-skipped reason={}", skip.token());
                status = RenderStatus::Skipped(skip.token());
            }
            Ok((rt, rb)) => {
                eprintln!("webgrab: info=auto-render reason={reason} chars={}", stage.visible);
                match render::render(&final_url, &render_options(cli, rt, rb)).await {
                    Ok(dom) => match build_stage(cli, &dom, &final_url) {
                        Ok(rs) => {
                            rendered_chars = Some(rs.visible);
                            status = choose_result(stage.visible, rs.visible);
                            if status.is_rendered() {
                                stage = rs;
                            } else {
                                eprintln!("webgrab: warn=auto-render-no-gain reason=shorter");
                            }
                        }
                        Err(e) => match fallback_reason(Phase::Extract, &e) {
                            Some(r) => {
                                eprintln!("webgrab: warn=auto-render-failed reason={r}");
                                eprintln!("{}", crate::error::sanitize_detail(&e.message));
                                status = RenderStatus::Failed(r);
                            }
                            None => return Err(e),
                        },
                    },
                    Err(e) => match fallback_reason(Phase::Render, &e) {
                        Some(r) => {
                            eprintln!("webgrab: warn=auto-render-failed reason={r}");
                            eprintln!("{}", crate::error::sanitize_detail(&format!("{} {}", e.message, e.detail.as_deref().unwrap_or(""))));
                            status = RenderStatus::Failed(r);
                        }
                        None => return Err(e),
                    },
                }
            }
        }
    }

    // 6. 空本文チェック（--rawは免除、設計§4.3 4）
    if !cli.raw && stage.body.trim().is_empty() {
        let (tok, prose) = hint_for(status);
        return Err(WebgrabError::new(ExitCode::Empty, format!("empty body extracted; retry with {prose}")).with_token("hint", tok));
    }

    // 7. 文字量制御・トークン
    let slice = budget::slice(&stage.body, cli.start_index, cli.max_chars);
    let max_chars_zero = cli.max_chars == 0;
    let tok = if cli.no_tokens { None } else { Some(tokens::count(&slice.content)) };

    // 8. 短い本文の通知（提案はrender_status基準）
    let content_len = slice.content.chars().count();
    let (short_content, short_content_suggest) = if !cli.raw && content_len > 0 && slice.total < SHORT_CONTENT_CHARS {
        let (hint, suggest) = hint_for(status);
        eprintln!("webgrab: warn=short-content chars={} hint={hint}", slice.total);
        (Some(slice.total), suggest)
    } else {
        (None, "")
    };

    let meta = Meta {
        title: stage.title,
        url: final_url,
        published_time: stage.published,
        tokens: tok,
        short_content,
        short_content_suggest,
        fence: cli.fence,
        render_status: status,
        static_chars,
        rendered_chars,
    };
    let extra = cli::extra_flags(cli, status);
    Ok(output::render(to_format(cli.format), &meta, &slice, max_chars_zero, &extra))
}
```

`if ... && let Some(..)`のletチェインはedition 2024で有効（`output.rs:188`に前例あり）。

- [ ] Step 4:確認

Run: `cargo test && cargo clippy --all-targets -- -D warnings`
Expected:緑（単体 + 統合12件）

- [ ] Step 5:コミット

```bash
git add src/pipeline.rs tests/integration.rs
git commit -m "feat(pipeline): --auto-renderの3フェーズ化、終了コード6のhintトークン、skip契約

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 11: E2Eハーネスとfixture

Files:
- Create: `tests/common/mod.rs`
- Create: `tests/fixtures/big_gzip.html.gz`, `tests/fixtures/README.md`
- Create: `tests/render_e2e.rs`

Interfaces:
- Produces（`tests/common/mod.rs`）を次に示す。
```rust
#![allow(dead_code)]
pub struct Route { pub path: &'static str, pub body: Vec<u8>, pub content_type: &'static str, pub headers: Vec<(&'static str, String)>, pub delay_ms: u64 }
pub struct Server { pub port: u16 }
pub fn start(routes: Vec<Route>) -> Server   // 常駐スレッド、未知パスは404、Connection: close
pub fn e2e_enabled() -> bool                 // WEBGRAB_E2E=1→true / CI設定かつ未設定→panic / それ以外→eprintln+false
pub static E2E_LOCK: std::sync::Mutex<()>
pub fn webgrab(args: &[&str]) -> (i32, String, String)  // WEBGRAB_CHROME→--chrome-path、WEBGRAB_E2E_NO_SANDBOX=1→--no-sandbox を付ける（E8はskip_chrome_envで無効化）
pub fn webgrab_raw(args: &[&str]) -> (i32, String, String)  // 環境変数由来の付加なし
pub const SENTINEL_FAST/SLOW/XHR/STATIC/SHORT/GZIP/DOM: &str
pub fn csr_fast() -> Route; csr_slow(); csr_xhr() -> Vec<Route>; static_article(); short_static(); big_gzip(); dom_bomb()
```

- [ ] Step 1: fixtureを生成

```bash
mkdir -p tests/fixtures
python3 -c "import sys;sys.stdout.write('<html><head><title>gz</title></head><body><article><p>'+'x'*2097152+' SENTINEL_GZIP_9f3c</p></article></body></html>')" | gzip -9 > tests/fixtures/big_gzip.html.gz
ls -l tests/fixtures/big_gzip.html.gz   # 約4KiB
```

`tests/fixtures/README.md`:

```markdown
# テスト fixture

- `big_gzip.html.gz`: 展開後約2MiB（`x`の繰り返し + 番兵 `SENTINEL_GZIP_9f3c`）を `gzip -9` した約4KiBのバイナリ。E13/E14 で `Content-Encoding: gzip` として配信し、`--max-bytes` が展開後バイトで効くことを検証する。再生成コマンドは次のとおり。

```sh
python3 -c "import sys;sys.stdout.write('<html><head><title>gz</title></head><body><article><p>'+'x'*2097152+' SENTINEL_GZIP_9f3c</p></article></body></html>')" | gzip -9 > tests/fixtures/big_gzip.html.gz
```
```

- [ ] Step 2: `tests/common/mod.rs`を書く

```rust
#![allow(dead_code)]
//! E2E/統合テスト共通: 常駐HTTPサーバ、E2Eゲート、fixture。

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::Command;
use std::sync::Mutex;
use std::thread;
use std::time::Duration;

pub struct Route {
    pub path: &'static str,
    pub body: Vec<u8>,
    pub content_type: &'static str,
    pub headers: Vec<(&'static str, String)>,
    pub delay_ms: u64,
}

impl Route {
    pub fn html(path: &'static str, body: impl Into<String>) -> Self {
        Route { path, body: body.into().into_bytes(), content_type: "text/html; charset=utf-8", headers: vec![], delay_ms: 0 }
    }
}

pub struct Server {
    pub port: u16,
}

impl Server {
    pub fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{}", self.port, path)
    }
}

/// 任意回数の要求に応答する常駐サーバ。スレッドはプロセス終了まで生きる。
pub fn start(routes: Vec<Route>) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let routes: Vec<(String, Vec<u8>, &'static str, Vec<(&'static str, String)>, u64)> = routes
                .iter()
                .map(|r| (r.path.to_string(), r.body.clone(), r.content_type, r.headers.clone(), r.delay_ms))
                .collect();
            thread::spawn(move || serve_one(stream, &routes));
        }
    });
    Server { port }
}

fn serve_one(mut stream: TcpStream, routes: &[(String, Vec<u8>, &'static str, Vec<(&'static str, String)>, u64)]) {
    let mut buf = [0u8; 8192];
    let n = stream.read(&mut buf).unwrap_or(0);
    let req = String::from_utf8_lossy(&buf[..n]);
    let path = req.lines().next().and_then(|l| l.split_whitespace().nth(1)).unwrap_or("/").to_string();
    let path_only = path.split('?').next().unwrap_or("/");
    match routes.iter().find(|r| r.0 == path_only) {
        Some((_, body, ct, headers, delay)) => {
            if *delay > 0 {
                thread::sleep(Duration::from_millis(*delay));
            }
            let mut head = format!("HTTP/1.1 200 OK\r\nContent-Type: {ct}\r\nContent-Length: {}\r\nConnection: close\r\n", body.len());
            for (k, v) in headers {
                head.push_str(&format!("{k}: {v}\r\n"));
            }
            head.push_str("\r\n");
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(body);
        }
        None => {
            let _ = stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        }
    }
    let _ = stream.flush();
}

pub static E2E_LOCK: Mutex<()> = Mutex::new(());

/// WEBGRAB_E2E=1で有効。CI下で未設定なら失敗、それ以外は理由を出してskip。
pub fn e2e_enabled() -> bool {
    if std::env::var("WEBGRAB_E2E").as_deref() == Ok("1") {
        return true;
    }
    if std::env::var("CI").is_ok() {
        panic!("WEBGRAB_E2E must be set to 1 in CI (render E2E would be silently skipped)");
    }
    eprintln!("skip: set WEBGRAB_E2E=1 to run render E2E (requires Chrome)");
    false
}

fn env_args() -> Vec<String> {
    let mut v = Vec::new();
    if let Ok(p) = std::env::var("WEBGRAB_CHROME") {
        v.push("--chrome-path".into());
        v.push(p);
    }
    if std::env::var("WEBGRAB_E2E_NO_SANDBOX").as_deref() == Ok("1") {
        v.push("--no-sandbox".into());
    }
    v
}

fn run(args: Vec<String>) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_webgrab")).args(&args).output().expect("binary runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

/// 環境変数由来のフラグ（--chrome-path / --no-sandbox）を付けて実行する。
pub fn webgrab(args: &[&str]) -> (i32, String, String) {
    let mut v: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    v.extend(env_args());
    run(v)
}

/// 環境変数を無視して実行する（E8用）。
pub fn webgrab_raw(args: &[&str]) -> (i32, String, String) {
    run(args.iter().map(|s| s.to_string()).collect())
}

pub const SENTINEL_FAST: &str = "SENTINEL_FAST_1a2b";
pub const SENTINEL_SLOW: &str = "SENTINEL_SLOW_3c4d";
pub const SENTINEL_XHR: &str = "SENTINEL_XHR_5e6f";
pub const SENTINEL_STATIC: &str = "SENTINEL_STATIC_7a8b";
pub const SENTINEL_SHORT: &str = "SENTINEL_SHORT_9c0d";
pub const SENTINEL_GZIP: &str = "SENTINEL_GZIP_9f3c";
pub const SENTINEL_DOM: &str = "SENTINEL_DOM_2e4f";

fn article(sentinel: &str) -> String {
    let para = "これはJavaScriptで後から挿入された本文です。抽出アルゴリズムが本文と認識できる十分な長さの日本語文章を用意しています。さらに文章を続けて厚みを持たせます。";
    format!("<article><h1>記事 {sentinel}</h1><p>{para}</p><p>{para}</p><p>{para}</p><p>{para}</p></article>")
}

fn csr(path: &'static str, delay_ms: u64, placeholder: &str, sentinel: &str) -> Route {
    let art = article(sentinel).replace('\'', "\\'");
    Route::html(path, format!(
        "<html><head><meta charset=\"utf-8\"><title>CSR</title></head><body><div id=\"app\">{placeholder}</div>\
         <script>setTimeout(function(){{document.getElementById('app').innerHTML='{art}';}},{delay_ms});</script></body></html>"
    ))
}

pub fn csr_fast() -> Route { csr("/csr_fast", 500, "", SENTINEL_FAST) }
pub fn csr_slow() -> Route { csr("/csr_slow", 2500, "読み込み中...", SENTINEL_SLOW) }

pub fn csr_xhr() -> Vec<Route> {
    let page = Route::html("/csr_xhr", "<html><head><meta charset=\"utf-8\"><title>XHR</title></head><body><div id=\"app\"></div>\
        <script>fetch('/api/data').then(function(r){return r.text()}).then(function(t){document.getElementById('app').innerHTML=t;});</script></body></html>");
    let api = Route { path: "/api/data", body: article(SENTINEL_XHR).into_bytes(), content_type: "text/html; charset=utf-8", headers: vec![], delay_ms: 1000 };
    vec![page, api]
}

pub fn static_article() -> Route {
    Route::html("/static", format!("<html><head><meta charset=\"utf-8\"><title>static</title></head><body>{}</body></html>", article(SENTINEL_STATIC)))
}

/// 静的150文字前後の本文。JSは20文字のシェルに置換する（JSチャレンジ模擬）。
pub fn short_static() -> Route {
    Route::html("/short", format!(
        "<html><head><meta charset=\"utf-8\"><title>short</title></head><body><article id=\"a\"><p>これは百五十文字程度の短い本文です {SENTINEL_SHORT}。抽出器が本文として認識できる長さはありますが二百文字には届きません。エスカレーション判定の境界を確認するための固定文です。末尾。</p></article>\
         <script>document.getElementById('a').innerHTML='<p>Please enable JS.</p>';</script></body></html>"
    ))
}

pub fn big_gzip() -> Route {
    Route {
        path: "/big_gzip",
        body: include_bytes!("../fixtures/big_gzip.html.gz").to_vec(),
        content_type: "text/html; charset=utf-8",
        headers: vec![("Content-Encoding", "gzip".to_string())],
        delay_ms: 0,
    }
}

/// 1KiBの文書で、JSがネットワークを経ずに3MiBのDOMを作る。
pub fn dom_bomb() -> Route {
    Route::html("/dom_bomb", format!(
        "<html><head><meta charset=\"utf-8\"><title>dom</title></head><body><div id=\"app\">{SENTINEL_DOM}</div>\
         <script>var s='<p>'+'y'.repeat(1048576)+'</p>';document.getElementById('app').innerHTML=s+s+s;</script></body></html>"
    ))
}
```

- [ ] Step 3: `tests/render_e2e.rs`を書く（E1〜E15）

```rust
//! render経路のE2E（設計 08 §6）。実Chromeとローカルサーバで検証する。WEBGRAB_E2E=1で有効。
mod common;
use common::*;

macro_rules! e2e {
    ($name:ident, $body:block) => {
        #[test]
        fn $name() {
            if !e2e_enabled() { return; }
            let _g = E2E_LOCK.lock().unwrap_or_else(|p| p.into_inner());
            $body
        }
    };
}

fn all_routes() -> Vec<Route> {
    let mut v = vec![csr_fast(), csr_slow(), static_article(), short_static(), big_gzip(), dom_bomb()];
    v.extend(csr_xhr());
    v
}

e2e!(e1_render_csr_fast, {
    let s = start(all_routes());
    let (code, out, err) = webgrab(&[&s.url("/csr_fast"), "--render", "--allow-private", "--no-robots"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains(SENTINEL_FAST), "{out}");
});

e2e!(e2_render_csr_slow_waits_past_old_fixed_sleep, {
    let s = start(all_routes());
    let (code, out, err) = webgrab(&[&s.url("/csr_slow"), "--render", "--allow-private", "--no-robots", "--wait-ms", "8000"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains(SENTINEL_SLOW), "{out}");
});

e2e!(e3_render_xhr, {
    let s = start(all_routes());
    let (code, out, err) = webgrab(&[&s.url("/csr_xhr"), "--render", "--allow-private", "--no-robots"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains(SENTINEL_XHR), "{out}");
});

e2e!(e4_auto_render_escalates_on_empty_shell, {
    let s = start(all_routes());
    let (code, out, err) = webgrab(&[&s.url("/csr_fast"), "--auto-render", "--allow-private", "--no-robots"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains(SENTINEL_FAST), "{out}");
    assert!(err.contains("info=auto-render reason=empty"), "{err}");
    assert!(!out.contains("short-content"), "{out}");
});

e2e!(e5_auto_render_does_not_launch_for_static_article, {
    let s = start(all_routes());
    let (code, out, err) = webgrab(&[&s.url("/static"), "--auto-render", "--allow-private", "--no-robots"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains(SENTINEL_STATIC));
    assert!(!err.contains("auto-render"), "{err}");
});

e2e!(e6_json_continue_command_uses_render, {
    let s = start(all_routes());
    let (code, out, err) = webgrab(&[&s.url("/csr_fast"), "--auto-render", "--allow-private", "--no-robots", "--format", "json", "--max-chars", "50"]);
    assert_eq!(code, 0, "{err}");
    let v: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
    assert_eq!(v["render_status"], "rendered");
    let cc = v["continue_command"].as_str().unwrap();
    assert!(cc.contains("--render") && !cc.contains("--auto-render"), "{cc}");
});

e2e!(e7_wait_cap_is_enforced, {
    let s = start(all_routes());
    let (code, out, err) = webgrab(&[&s.url("/csr_slow"), "--render", "--allow-private", "--no-robots", "--wait-ms", "1500"]);
    assert!(!out.contains(SENTINEL_SLOW), "{out}");
    let ok = (code == 0 && out.contains("[webgrab:short-content")) || (code == 6 && err.contains("error=empty"));
    assert!(ok, "code={code} out={out} err={err}");
});

e2e!(e8_auto_render_falls_back_when_chrome_missing, {
    let s = start(all_routes());
    let (code, _out, err) = webgrab_raw(&[&s.url("/csr_fast"), "--auto-render", "--allow-private", "--no-robots", "--chrome-path", "/nonexistent/chrome"]);
    assert_eq!(code, 6, "{err}");
    assert!(err.lines().any(|l| l.starts_with("webgrab: error=empty hint=--raw")), "{err}");
    assert!(err.lines().any(|l| l.starts_with("webgrab: warn=auto-render-failed reason=render")), "{err}");
});

e2e!(e9_render_refuses_internal_without_allow_private, {
    let s = start(all_routes());
    let (code, _out, err) = webgrab(&[&s.url("/csr_fast"), "--render", "--no-robots"]);
    assert_eq!(code, 8, "{err}");
    assert!(err.contains("error=netguard"), "{err}");
});

e2e!(e10a_no_gain_keeps_static_result, {
    let s = start(all_routes());
    let (code, out, err) = webgrab(&[&s.url("/short"), "--auto-render", "--allow-private", "--no-robots"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains(SENTINEL_SHORT), "{out}");
    assert!(out.contains("[webgrab:render-status no-gain reason=shorter]"), "{out}");
    assert!(err.contains("warn=auto-render-no-gain reason=shorter"), "{err}");
});

e2e!(e10b_no_gain_json_char_counts, {
    let s = start(all_routes());
    let (code, out, err) = webgrab(&[&s.url("/short"), "--auto-render", "--allow-private", "--no-robots", "--format", "json"]);
    assert_eq!(code, 0, "{err}");
    let v: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
    assert_eq!(v["render_status"], "no-gain");
    assert!(v["rendered_chars"].as_u64().unwrap() < v["static_chars"].as_u64().unwrap(), "{out}");
});

e2e!(e11_max_chars_zero_still_escalates, {
    let s = start(all_routes());
    let (code, out, err) = webgrab(&[&s.url("/csr_fast"), "--auto-render", "--allow-private", "--no-robots", "--format", "json", "--max-chars", "0"]);
    assert_eq!(code, 0, "{err}");
    let v: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
    assert_eq!(v["render_status"], "rendered");
    assert_eq!(v["markdown"], "");
});

e2e!(e12_raw_escalates_on_visible_text, {
    let s = start(all_routes());
    let (code, out, err) = webgrab(&[&s.url("/csr_fast"), "--auto-render", "--raw", "--allow-private", "--no-robots", "--format", "json"]);
    assert_eq!(code, 0, "{err}");
    let v: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
    assert_eq!(v["render_status"], "rendered");
    assert!(v["markdown"].as_str().unwrap().contains(SENTINEL_FAST));
});

e2e!(e13_render_max_bytes_counts_decoded_bytes, {
    let s = start(all_routes());
    let (code, _out, err) = webgrab(&[&s.url("/big_gzip"), "--render", "--allow-private", "--no-robots", "--max-bytes", "1048576"]);
    assert_eq!(code, 4, "{err}");
    assert!(err.contains("error=http"), "{err}");
});

e2e!(e14_static_phase_error_propagates_under_auto_render, {
    let s = start(all_routes());
    let (code, _out, err) = webgrab(&[&s.url("/big_gzip"), "--auto-render", "--allow-private", "--no-robots", "--max-bytes", "1048576"]);
    assert_eq!(code, 4, "{err}");
    assert!(err.contains("error=http"), "{err}");
});

e2e!(e15_dom_bomb_is_capped_before_content, {
    let s = start(all_routes());
    let (code, _out, err) = webgrab(&[&s.url("/dom_bomb"), "--render", "--allow-private", "--no-robots", "--max-bytes", "1048576"]);
    assert_eq!(code, 4, "{err}");
    assert!(err.contains("error=http"), "{err}");
});
```

- [ ] Step 4:実行

Run: `WEBGRAB_E2E=1 cargo test --test render_e2e -- --test-threads=1`
Expected: 15件PASS。落ちたら`--nocapture`でstderrを見て、`render.rs`を直す（設計から外れない範囲）。

- [ ] Step 5: `cargo clippy --all-targets -- -D warnings`と`cargo fmt`

- [ ] Step 6:コミット

```bash
git add tests/common tests/fixtures tests/render_e2e.rs
git commit -m "test: 実Chromeを使うrender E2E（E1〜E15）とローカルfixtureを追加

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 12: CI workflow

Files:
- Create: `.github/workflows/ci.yml`

- [ ] Step 1: SHAを解決

```bash
for r in actions/checkout:v4 dtolnay/rust-toolchain:stable Swatinem/rust-cache:v2 taiki-e/install-action:cargo-llvm-cov; do
  repo=${r%%:*}; ref=${r##*:}
  echo "$repo@$(gh api repos/$repo/commits/$ref --jq .sha) # $ref"
done
```

- [ ] Step 2: `.github/workflows/ci.yml`を書く（`<SHA>`はStep 1の値）

```yaml
name: CI
on:
  push:
    branches: [master]
  pull_request:
permissions:
  contents: read
concurrency:
  group: ci-${{ github.workflow }}-${{ github.ref }}
  cancel-in-progress: ${{ github.event_name == 'pull_request' }}
jobs:
  check:
    runs-on: ubuntu-24.04
    timeout-minutes: 20
    steps:
      - uses: actions/checkout@<SHA> # v4
        with:
          persist-credentials: false
      - uses: dtolnay/rust-toolchain@<SHA> # stable
        with:
          components: rustfmt, clippy
      - uses: Swatinem/rust-cache@<SHA> # v2
      - run: '! grep -nE "uses: .*@(v[0-9]|stable|cargo-llvm-cov)" .github/workflows/ci.yml | grep -v reusable-workflows'
      - run: grep -q "persist-credentials: false" .github/workflows/ci.yml
      - run: cargo fmt --check
      - run: cargo clippy --all-targets -- -D warnings
      - run: python3 tools/doclint.py docs/
  test:
    runs-on: ubuntu-24.04
    timeout-minutes: 30
    env:
      WEBGRAB_E2E: "1"
      WEBGRAB_E2E_NO_SANDBOX: "1"
    steps:
      - uses: actions/checkout@<SHA> # v4
        with:
          persist-credentials: false
      - uses: dtolnay/rust-toolchain@<SHA> # stable
      - uses: Swatinem/rust-cache@<SHA> # v2
      - run: google-chrome --version
      - run: cargo test --lib --bins --test integration
      - run: cargo test --test render_e2e -- --test-threads=1
  coverage:
    runs-on: ubuntu-24.04
    timeout-minutes: 30
    env:
      WEBGRAB_E2E: "1"
      WEBGRAB_E2E_NO_SANDBOX: "1"
    steps:
      - uses: actions/checkout@<SHA> # v4
        with:
          persist-credentials: false
      - uses: dtolnay/rust-toolchain@<SHA> # stable
        with:
          components: llvm-tools-preview
      - uses: taiki-e/install-action@<SHA> # cargo-llvm-cov
      - uses: Swatinem/rust-cache@<SHA> # v2
      - run: google-chrome --version
      - run: cargo llvm-cov --lib --bins --test integration --test render_e2e --fail-under-lines 80 -- --test-threads=1
  security:
    permissions:
      contents: read
      pull-requests: write
    uses: okamyuji/reusable-workflows/.github/workflows/security-scan.yml@v1
```

- [ ] Step 3:ローカルで同じgrepとcoverageを実行

```bash
! grep -nE "uses: .*@(v[0-9]|stable|cargo-llvm-cov)" .github/workflows/ci.yml | grep -v reusable-workflows
WEBGRAB_E2E=1 cargo llvm-cov --lib --bins --test integration --test render_e2e --fail-under-lines 80 -- --test-threads=1 2>&1 | tail -5
```
Expected: grepは無出力（終了0）、coverageは`TOTAL`行が80%以上。80未満ならGlobal Constraint 5の二択で`--ignore-filename-regex 'render\.rs'`をYAMLに戻し、差分をバックログに記録する。

- [ ] Step 4:コミット

```bash
git add .github/workflows/ci.yml
git commit -m "ci: fmt/clippy/doclint/test(E2E)/coverage/security-scan のworkflowを追加

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 13:文書更新（04 v1.3、README、samples、backlog）

Files:
- Modify: `docs/04-design.md`（設計08 §4.5の列挙どおり: §3決定表の暗黙フォールバック例外と`--auto-render`、§3.1第一層の範囲訂正と第二層の`dataReceived`層、§4のrender/robots/pipeline説明とモジュールツリーに`render/wait.rs`、§5フラグ表（`--auto-render`/`--wait-ms`/`--no-sandbox`/`--max-bytes`行）・機械可読行の種別`info=`・「先頭行」の定義・終了コード6の書式差・short-content提案規則、§6 json/frontmatter（`render_status`/`static_chars`/`rendered_chars`）・`--max-chars 0`の行・継続コマンド規則(1)例外と(4)、§7の写像とauto-render失敗時と終了コード8詳細行、§8 E2Eとカバレッジ80、§9、変更履歴1.3）
- Modify: `README.md`（使い方に`--auto-render`と`--wait-ms`、「セキュリティと信頼モデル」に`--no-sandbox`と`--auto-render`の注意）
- Modify: `samples/skills/claude/webgrab/SKILL.md`, `samples/skills/codex/AGENTS.md`, `samples/skills/kimi/webgrab-tool.md`（「使い分け」に`--auto-render`、`hint=`は`render_status`基準なのでそのまま使う、終了コード表の6行、`render_status`/`[webgrab:render-status ...]`の読み方、`no-gain`の意味、`--no-sandbox`が継続コマンドに載る注意、一覧ページ・連続取得では`--auto-render`を既定にしない）
- Modify: `src/render.rs:25`の`max_bytes` docコメント（Task 8で済んでいなければ）

- [ ] Step 1: 04-design.mdを編集しdoclintとja-styleを通す

Run: `python3 tools/doclint.py docs/ && node "$HOME/.claude/ja-style/check.js" scan docs/04-design.md README.md samples/skills/claude/webgrab/SKILL.md 2>&1 | grep -c error`
Expected: `Critical 0 / High 0`、error 0

- [ ] Step 2:コミット

```bash
git add docs/04-design.md README.md samples/ src/render.rs
git commit -m "docs: 設計書v1.3・README・エージェント向けSKILLを--auto-renderに合わせて更新

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 14:ブラウザ実動作検証（設計 §8）と07報告

Files:
- Modify: `docs/07-verification-report.md`（「JS描画改善の検証（2026-08-27）」節）
- Modify: `docs/_quality/IMPROVEMENT_BACKLOG.md`（カバレッジ二択の記録、skip閾値の実測結果）

Claude Code固有のブラウザツールは使わない。突合の相手は、同じChromeを`--headless=new --dump-dom`で直接起動して得たDOMの可視テキストとする（webgrabを経由しない独立経路）。

- [ ] Step 1:リリースビルドで取得し保存する

```bash
cargo build --release
V=/tmp/webgrab-verify; mkdir -p $V
CHROME="${WEBGRAB_CHROME:-/Applications/Google Chrome.app/Contents/MacOS/Google Chrome}"
for u in https://react.dev/learn https://demo.playwright.dev/todomvc/ https://qiita.com/; do
  n=$(echo "$u" | tr '/:' '__')
  { /usr/bin/time -p ./target/release/webgrab "$u" --render --format json > "$V/$n.render.json"; } 2> "$V/$n.render.err"
  { /usr/bin/time -p ./target/release/webgrab "$u" --auto-render --format json > "$V/$n.auto.json"; } 2> "$V/$n.auto.err"
  "$CHROME" --headless=new --disable-gpu --no-sandbox --virtual-time-budget=8000 --dump-dom "$u" > "$V/$n.dom.html" 2>/dev/null
done
```

- [ ] Step 2:独立経路のDOMから見出し1つと先頭段落を抜き出し、取得結果の`markdown`に含まれるか照合する

`tools/verify_render.py`を新設して実行する（リポジトリにコミットし、07から参照する）。

```python
#!/usr/bin/env python3
"""webgrab --render の取得結果を、独立経路（headless Chrome --dump-dom）の可視テキストと突合する。
使い方: python3 tools/verify_render.py /tmp/webgrab-verify
"""
import sys, json, glob, re
from html.parser import HTMLParser


class Extract(HTMLParser):
    def __init__(self):
        super().__init__()
        self.headings, self.paras, self.cur, self.skip = [], [], None, 0

    def handle_starttag(self, tag, attrs):
        if tag in ("script", "style", "noscript"):
            self.skip += 1
        if tag in ("h1", "h2"):
            self.cur = ("h", "")
        elif tag == "p":
            self.cur = ("p", "")

    def handle_endtag(self, tag):
        if tag in ("script", "style", "noscript"):
            self.skip = max(0, self.skip - 1)
        if self.cur and tag in ("h1", "h2", "p"):
            kind, txt = self.cur
            txt = re.sub(r"\s+", " ", txt).strip()
            (self.headings if kind == "h" else self.paras).append(txt)
            self.cur = None

    def handle_data(self, data):
        if self.skip == 0 and self.cur:
            self.cur = (self.cur[0], self.cur[1] + data)


def main(root):
    rows = []
    for dom in sorted(glob.glob(f"{root}/*.dom.html")):
        base = dom[: -len(".dom.html")]
        ex = Extract()
        ex.feed(open(dom, encoding="utf-8", errors="replace").read())
        head = next((x for x in ex.headings if len(x) >= 4), "")
        para = next((x for x in ex.paras if len(x) >= 30), "")
        for mode in ("render", "auto"):
            try:
                j = json.load(open(f"{base}.{mode}.json", encoding="utf-8"))
            except Exception as e:  # 出力なし（終了コード非0）
                rows.append((base.rsplit("/", 1)[-1], mode, head[:40], para[:40], "NO-OUTPUT", str(e)[:60]))
                continue
            md = j.get("markdown", "")
            ok_h = head != "" and head in md
            ok_p = para != "" and para[:30] in md
            verdict = f"heading={'一致' if ok_h else '不一致'} paragraph={'一致' if ok_p else '不一致'}"
            rows.append((base.rsplit("/", 1)[-1], mode, head[:40], para[:40], j.get("render_status"), verdict))
    print("| URL | mode | 見出し | 先頭段落(先頭40字) | render_status | 判定 |")
    print("|---|---|---|---|---|---|")
    for r in rows:
        print("| " + " | ".join(str(x) for x in r) + " |")


if __name__ == "__main__":
    main(sys.argv[1] if len(sys.argv) > 1 else "/tmp/webgrab-verify")
```

Run: `python3 tools/verify_render.py /tmp/webgrab-verify`
判定は「見出しが完全一致」かつ「先頭段落の先頭30文字が含まれる」。SPAのルーティングで独立経路の`--dump-dom`が本文を含まない場合は、`--virtual-time-budget=20000`で再取得して結果を記録する。

- [ ] Step 3:Chromeコールドスタート実測とskip閾値の判定

`render.rs`の`render_inner`で`Browser::launch`の前後に一時的な`eprintln!("webgrab: info=render-launch-ms={}", t.elapsed().as_millis())`を入れ、`./target/release/webgrab https://example.com --render`を5回実行して中央値Lを取り、その行を削除する（コミットしない）。判定は`5000 >= L + 2000 + 1000`ならskip閾値5秒は妥当。満たさなければ`src/pipeline.rs`の`SKIP_TIMEOUT_MIN`を`L + 3000ms`へ切り上げ、設計書§3決定表のskip閾値行と§4.3手順2の数値を同じ値に更新する。

- [ ] Step 4b:SKILL経由の呼び出し確認（Claude Codeから）

`cargo install --path .`でPATHの`webgrab`を更新し、`~/.claude/skills/webgrab/SKILL.md`をTask 13で更新した`samples/skills/claude/webgrab/SKILL.md`に差し替える（`diff -u`で差分を確認してから`cp`）。次にClaude Codeのセッションで`webgrab-fetch`スキルを起動し、スキル本文が指示するコマンド形（既定呼び出し、`--auto-render --format json`、一覧向け`--raw`、`[webgrab:truncated ...]`の継続コマンド）をJS描画ページ（`https://demo.playwright.dev/todomvc/`と`https://react.dev/learn`）に対して実行する。確認項目は、(a) 既定呼び出しで`render_status`が`rendered`または`static`になり本文が取れること、(b) `--format json`の`render_status` / `static_chars` / `rendered_chars`が出ること、(c) 終了コード6のときの`hint=`がスキル本文の説明と一致すること、(d) 継続コマンドに`--render`と描画系フラグが再現されること。結果はStep 4の表に「SKILL経由」列として加える。

- [ ] Step 4:07に表で記録する

記録項目は次のとおり。Step 2の表（URL / 比較文字列 / 一致 / 終了コード / `render_status` / 早期終了か上限到達か。後者は`.err`の`time`のrealと`--wait-ms`既定5000の比較で判定）、Lの実測値とskip閾値の判定、CIのsandbox観測（`WEBGRAB_E2E_NO_SANDBOX`が必要だったか）、coverageの二択（除外なしで80以上か、除外を戻したか）。

- [ ] Step 5:コミット

```bash
git add docs/07-verification-report.md docs/_quality/IMPROVEMENT_BACKLOG.md tools/verify_render.py src/pipeline.rs docs/08-js-render-design.md
git commit -m "docs: JS描画改善のブラウザ突合とCI観測を検証報告書に記録

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 15: PR作成とCI / CodeRabbit対応

- [ ] Step 1: pushとPR

```bash
git push -u origin feat/js-render-auto
gh pr create --base master --title "feat: JS描画ページの自動エスカレーション(--auto-render)と待機戦略・展開後バイト上限" --body-file <(cat <<'EOF'
## 概要
JS描画ページで本文が取れない問題（空シェル→internal error、固定2秒待ち）を、設計書 docs/08-js-render-design.md v1.6 に従って解決する。

- `--auto-render`（opt-in）: 静的取得の可視テキストが200文字未満なら同一プロセスでChromeへ切替。失敗時は静的結果へ復帰し `render_status` で通知
- 待機: ネットワーク静止 + DOM安定 + 可視テキスト200文字以上で早期終了、`--wait-ms` は上限（既定5000）
- `--max-bytes` を両経路とも展開後バイトで統一（render は `Network.dataReceived` + DOM長）
- 終了コード8はrenderフェーズの他の失敗より優先、iframeでは立てない
- E2E（実Chrome、ローカルfixture）15件、CI新設

## テスト計画
- [ ] `cargo test`（単体・統合）
- [ ] `WEBGRAB_E2E=1 cargo test --test render_e2e -- --test-threads=1`
- [ ] `cargo llvm-cov ... --fail-under-lines 80`
- [ ] ブラウザ突合（docs/07-verification-report.md）

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)
```

- [ ] Step 2:CIを監視する

Run: `gh pr checks --watch --fail-fast`
Expected: 全ジョブ緑。赤なら`gh run view <run-id> --log-failed`で原因を特定し、直してpushし、再度watchする。sandboxが理由の失敗は`WEBGRAB_E2E_NO_SANDBOX`の有無を07に記録する。

- [ ] Step 3:CodeRabbitのレビューを待ち、指摘を全件処理する

CodeRabbitはPR作成後に非同期でレビューする（通常2〜10分）。次のループを、未解決スレッドがゼロになるまで繰り返す（最大5周。超えたら未解決一覧を最終報告に載せて止める）。

```bash
N=$(gh pr view --json number --jq .number)
# (a) レビュー到着待ち: coderabbitai[bot] のレビューまたはコメントが現れるまで60秒間隔で最大15分
for i in $(seq 1 15); do
  n=$(gh api "repos/okamyuji/webgrab/pulls/$N/reviews" --jq '[.[] | select(.user.login=="coderabbitai[bot]")] | length')
  c=$(gh api "repos/okamyuji/webgrab/pulls/$N/comments" --jq '[.[] | select(.user.login=="coderabbitai[bot]")] | length')
  if [ "$n" -gt 0 ] || [ "$c" -gt 0 ]; then break; fi
  sleep 60
done
# (b) 未解決スレッドの一覧
gh api graphql -f query='query($o:String!,$r:String!,$n:Int!){repository(owner:$o,name:$r){pullRequest(number:$n){reviewThreads(first:100){nodes{id isResolved path line comments(first:5){nodes{author{login} body databaseId}}}}}}}' -f o=okamyuji -f r=webgrab -F n=$N --jq '.data.repository.pullRequest.reviewThreads.nodes[] | select(.isResolved==false)'
```

各スレッドについて、(1) 指摘をコードと設計書で検証し、(2) 正しければ修正してコミット、(3) 誤りまたは設計上の意図なら理由を返信する（`gh api repos/okamyuji/webgrab/pulls/$N/comments/<databaseId>/replies -f body='...'`）。(4) 修正をpushしたら`gh pr checks --watch --fail-fast`で緑を確認し、(a)から繰り返す（pushごとにCodeRabbitは差分を再レビューする。`gh pr comment $N --body '@coderabbitai review'`で手動再実行できる）。(5) 返信済み・修正済みのスレッドはGraphQLの`resolveReviewThread`で解決にする。

```bash
gh api graphql -f query='mutation($id:ID!){resolveReviewThread(input:{threadId:$id}){thread{isResolved}}}' -f id=<threadId>
```

終了条件は「未解決スレッド0件かつCI緑」。

- [ ] Step 4:最終報告

`cargo install --path .`で`~/.cargo/bin/webgrab`を更新する案内、配置済みSKILLコピー（`~/.claude/skills/webgrab/SKILL.md`、`~/.kimi-code/AGENTS.md`）への反映手順を含める。
