# 取得忠実度の改善 設計書

- バージョン: 1.0
- 日付: 2026-09-21
- 対象: webgrab 0.1.0（[04-design.md](04-design.md)と[08-js-render-design.md](08-js-render-design.md)への差分設計）
- 根拠: 本書§1の再現ログ、`src/pipeline.rs`、`src/fetch.rs`、`src/decode.rs`、`src/budget.rs`、`src/render/world.rs`の現行実装、reqwest 0.13.4のソース（`src/async_impl/client.rs`）、url 2.5.8のソース（`src/parser.rs`）

## 1. 目的と観測事実

webgrabの目的は、ページの内容を欠落なくLLMへ渡すことです。実URLで計測したところ、HTML以外の入力やリダイレクトを伴う取得で、内容の欠落やリンクの破損が生じていました。再現した事象を次の表に示します。

| # | 再現手順 | 観測 | 原因 |
|---|---|---|---|
| G1 | `text/plain`のRustソース（tokioの`mutex.rs`）を既定で取得 | 1396行が1行になり、`Mutex<T>`は17個が0個、`<T: ?Sized>`は12個が0個。終了コード0で警告なし。`--raw`と`--format text`でも同じ | `pipeline::run`が`content_type`を`decode`にしか渡さず、`text/plain`もdom_smoothieとhtmdを通る。`<T>`がHTMLタグとして解釈され、空白が畳まれる。04-design.md §7の「text/plainはそのまま出力」に実装が合っていない |
| G2 | `https://docs.rs/tokio`を`--render`で取得 | `URL Source`が`https://docs.rs/tokio`のまま。本文のリンクが`https://docs.rs/runtime/index.html`になり、確認した6本中5本がHTTP 400。静的取得は`https://docs.rs/tokio/latest/tokio/`を返す | render経路が`final_url`に要求URLを入れており、相対リンクの解決基準がリダイレクト前のURLになる |
| G3 | `https://crates.io/crates/tokio`を既定で取得 | HTTP 404で終了コード4。curlでも`Accept`なしと`Accept: */*`は404、`Accept: text/html,application/xhtml+xml`は200 | 静的経路がreqwest既定の`Accept: */*`をそのまま送っており、HTMLを優先していない |
| G4 | Rust Bookの1章（17544文字）を`--max-chars`1500〜19500の21通りで取得 | 21回中18回が行の途中で切れ、4回がコードフェンスの内側で切れる | `budget::slice`が`start + max_chars`の文字位置でそのまま切る |
| G5 | stackoverflow.comの質問ページを既定で取得 | HTTP 403で終了コード4。`--render`は4340文字を返すが、stderrに再試行の手掛かりがない | 403のエラーメッセージに提案が含まれない |

## 2. スコープ

- 対象: (a) `text/plain`の素通し、(b) render後URLの採用、(c) 静的経路の`Accept`ヘッダ、(d) 切り詰め位置の改行境界への調整、(e) HTTP 403時の`--render`提案
- 対象外: `application/json`や`text/markdown`など`text/plain`以外の非HTML形式の受け入れ（04-design.md §10を維持）、HTTPエラー時の`--auto-render`エスカレーション（08-js-render-design.md §4.3の手順1を維持）、コードフェンスを認識した切り詰め、`Accept-Language`の送出、Content-Type欠落時の内容推定

## 3. 決定表（MECE、全行確定済み）

| 決定 | 選択肢 | 採用 | 理由 |
|---|---|---|---|
| `text/plain`の本文生成 | HTML経路のまま / 素通し / コードフェンスで包む | 素通し | 04-design.md §7の契約どおり。READMEや`llms.txt`は既にMarkdownであり、フェンスで包むと構造が失われる |
| `text/plain`の判定 | Content-Typeの主タイプ / 拡張子 / 内容推定 | Content-Typeの主タイプが`text/plain`（大文字小文字を無視）。Content-Type欠落は従来どおりHTML経路 | 判定根拠がサーバの宣言だけで済み、誤判定の経路が増えない |
| `text/plain`の文字コード判定 | 従来の3段 / HTML metaの走査を省く2段 | ヘッダのcharset、chardetng推定の2段 | 本文がHTMLを含むソースコードのとき、本文中の`<meta charset>`を採用すると文字化けする。charsetなしの`text/plain`として誤配信されたHTMLは推定だけに頼ることになるが、G1の実害を優先する |
| `text/plain`の危険リンクスキーム | 無害化しない / Markdownのリンク記法だけ無害化 / `<`をエスケープ | `convert::sanitize_link_schemes`を、インラインリンク、オートリンク、参照定義の3形に対応させて適用 | READMEの「危険リンクスキーム無害化」は出力全体の契約である。`<`のエスケープは`Mutex<T>`を壊し、G1の欠落を再び生むため採らない。したがって生のHTMLタグは無害化の対象外とする |
| `text/plain`の空本文と短文 | 終了コード6とshort-contentを適用 / `--raw`と同じく免除 | 免除。空本文のための通知は足さない | 抽出を行わないため「抽出結果0文字」に当たらない。`--render`や`--raw`の提案は効果がない。空の入力に空の出力を返す挙動は、`--raw`の空本文と同じである |
| `text/plain`と`--auto-render` | 判定対象 / 対象外 | 対象外（`render_status=static`） | Chromeで描画しても同じテキストしか得られない |
| render後URLの取得方法 | 要求URLのまま / DOMと別の評価 / DOMと同じ評価で同時取得 | 分離ワールドの1回の評価で`[location.href, DOM HTML]`を同時に取得 | URLとDOMが同じ時点の値になる。評価の回数と時間予算が増えない |
| render後URLの検証 | 無検証 / 条件を満たす値だけ採用 | `url`クレートで解釈でき、スキームがhttpまたはhttpsの値からuserinfoを取り除き、その結果が8192バイト以下なら採用。それ以外は要求URLに戻す | `about:blank`や`chrome-error://`を`URL Source`に出さない。`Url::parse`はタブと改行を除去し、他の制御文字をパーセントエンコードする。userinfoを残すと`https://accounts.example@evil.test/`のように権威部を偽装した表示になる。長さの上限は、`history.pushState`で膨らませたURLが`--max-chars`の外でヘッダと継続コマンドを肥大させるのを防ぐ |
| render後URLのオリジンが要求URLと異なる場合 | 採用しない / 採用して警告 / 採用して通知なし | 採用し、通知は足さない | 本文の出どころを正しく示すことが目的であり、`URL Source`自体が通知になる。継続コマンドによる次回の取得は、そのURLに対するnetguardとrobots.txtの確認を改めて受ける |
| `--auto-render`時のURL | 常に静的経路の`final_url` / render結果を採用したときだけrender後URL | render結果を採用したとき（`rendered`）だけrender後URL。`no-gain`・`failed`・`skipped`は静的経路の`final_url` | 本文とURLの出どころを一致させる |
| `Accept`ヘッダ | reqwest既定の`*/*` / HTML優先の値を固定 / フラグで可変 | `text/html,application/xhtml+xml,text/plain;q=0.9,*/*;q=0.8`を静的経路の本文取得に固定で送る。robots.txtの取得は既定の`*/*`のまま | G3を解消する最小の変更。可変にする要求はない |
| 切り詰め位置 | 文字位置のまま / 直前の改行まで戻す / コードフェンスを認識 | 範囲の後半に改行があれば、最後の改行の直後まで戻す | 行の途中の切断を減らす。戻し幅を半分までに限ることで、1ページの量が極端に減らない。フェンス認識は戻し幅が読めないため採らない |
| 403時の通知 | なし / メッセージ末尾に`hint=--render` / 自動エスカレーション | メッセージ末尾に`hint=--render`を付ける | stderr先頭行の`error=http <message>`書式を保てる。エスカレーションの可否は08-js-render-design.md §4.3の手順1の決定を維持する |

## 4. 仕様

### 4.1 `text/plain`の素通し（G1）

静的経路で最終応答のContent-Typeの主タイプが`text/plain`のとき、`decode`の結果を本文にします。`extract`と`convert::to_markdown`は呼びません。`--raw`の有無と`--format`の値は本文に影響しません。`--format html`でも同じテキストを出します（04-design.md §6のhtml形式の例外）。

文字コードは、ヘッダのcharset、chardetng推定の2段で判定します。HTML metaの走査は行いません。デコードできないバイトの置換と`warn=decode-replacement`の通知は従来どおりです。

デコード後の本文に加わる変更は、次の3点だけです。

1. `convert::sanitize_link_schemes`が、インラインリンク（`](javascript:`）、オートリンク（`<javascript:`）、参照定義（`]: javascript:`）の3形にある危険スキームを無害化します。インラインリンクと参照定義では、区切りの直後の空白を読み飛ばして判定します。該当するスキームの前に`unsafe-`を挿入するだけで、文字は削りません。
2. `output::render`による端末制御文字の除去
3. `output::render`によるwebgrab制御マーカーの無害化（`[webgrab:`が`[quoted-webgrab:`になる）

生のHTMLタグ（`<a href="javascript:...">`や`<script>`）は変更しません。`text/plain`の出力をHTMLとして描画する消費者は、自身で無害化する必要があります。

そのほかの扱いは次のとおりです。

- `title`と`published_time`は値なしです。
- `static_chars`は本文の文字数です。他の経路では可視テキスト長ですが、`text/plain`では抽出HTMLが存在しないため、この値で置き換えます。
- 終了コード6の判定とshort-contentの通知は行いません（`--raw`と同じ扱い）。本文が0文字でも終了コード0で、stderrには何も出しません。このとき`--fence`を付けないtext形式とhtml形式のstdoutには本文もマーカーも出ず、markdown、frontmatter、jsonの各形式は文字数0を示します。
- `--auto-render`のエスカレーション判定は行わず、`render_status`は`static`のままです。
- `--start-index`と`--max-chars`による文字量制御、トークン概算、フッタは他の経路と同じです。

`--render`を明示した場合はChromeが返すDOMを扱うため、この節の対象外です。

### 4.2 render後URLの採用（G2）

`render::render`の戻り値を、DOM HTMLとrender後URLの組に変えます。08-js-render-design.md §4.2の手順6にある最後の評価は、`[location.href.slice(0, 8193), doctype + outerHTML]`の配列を返す式に置き換えます。評価そのものが失敗または時間切れになった場合と、配列の要素1（DOM HTML）が文字列でない場合は、従来どおり終了コード7です。

render後URLを決めるのは純関数`resolve_final_url(requested, reported)`です。`reported`を`url`クレートで解釈し、スキームがhttpまたはhttpsであることを確かめ、userinfo（ユーザー名とパスワード）を取り除きます。その結果の文字列が8192バイト以下なら、それを返します。配列の要素0が文字列でない場合と、いずれかの条件を満たさない場合は、`requested`が戻り値になります。評価式の`slice(0, 8193)`は転送量を抑えるためのものです。`reported`が8193バイト以上のときは、途中で切り詰められた可能性があるため採用しません。

`pipeline::run`は、このURLを次の3か所に使います。

1. `extract`の`base_url`（相対リンクの解決基準）。renderフェーズの抽出では常にrender後URLを使います。
2. `Meta.url`（`URL Source`、frontmatterとjsonの`url`）
3. 継続コマンドのURL

`--auto-render`では、render結果を採用したとき（`render_status=rendered`）だけ、2と3にrender後URLを使います。`no-gain`・`failed`・`skipped`のときは、renderフェーズの抽出結果ごと破棄し、静的経路の`final_url`を使います。

`--raw`は`base_url`を使わないため、`--render --raw`では相対リンクが相対のまま残ります。この場合に変わるのは2と3だけです。

render後URLは、サーバのリダイレクトや`location.replace`でオリジンごと変わりえます。`history.pushState`では同一オリジン内の任意の値になります。そのため、この値を信頼できない入力として扱います。出力時には`output::render`の行無害化を通り、継続コマンドでは`shell_quote`で囲まれます。継続コマンドを実行した次回の取得は、そのURLに対するnetguardとrobots.txtの確認を改めて受けます。今回の実行でChromeが辿った着地ホストのrobots.txtを確認しない点は、08-js-render-design.md §4.3の手順3にある既知の制約のままです。

### 4.3 `Accept`ヘッダ（G3）

`fetch::fetch`のクライアントに、`default_headers`で`Accept: text/html,application/xhtml+xml,text/plain;q=0.9,*/*;q=0.8`を設定します。reqwestの`default_headers`は`insert`で既定の`*/*`を置き換えるため、`Accept`は1値だけ送られます。クライアントはホップごとに作るので、リダイレクトの各ホップに同じ値が届きます。`robots_allowed`のクライアントは変更せず、robots.txtの要求は既定の`*/*`のままです。

### 4.4 切り詰め位置の調整（G4）

`budget::slice`は、`max_chars > 0`かつ切り詰めが発生するとき（`start`と`max_chars`の飽和加算が`total`より小さい）に限り、終端を次の規則で決めます。

1. 仮の終端を`end = start + max_chars`とします。
2. 範囲`[start, end)`にある最後の改行（`\n`）の位置を`i`とします。
3. `i + 1 >= start + ceil(max_chars / 2)`なら、終端を`i + 1`にします。条件を満たす改行がなければ、終端は`end`のままです。

`--max-chars`は上限であり、出力がそれより短くなることがあります。範囲表記は半開区間`[start, 実際の終端)`で、フッタ、`chars`、継続コマンドの`--start-index`はこの終端を指します。`max_chars > 0`のとき終端は常に`start`より大きいため、取得が止まることはありません。`budget::slice`の出力を継続の順に連結すると、スライス前の本文と一致します。出力時の無害化（§4.1の2と3）はスライスの後にページ単位で行うため、この一致はstdoutの連結については保証しません。`--max-chars 0`と最終ページの挙動は従来のままで、この調整はすべての`--format`に適用します。

次の場合は、調整後も行の途中で切れます。範囲の後半に改行がない場合（1行が`max_chars`の半分を超える場合）と、最終ページの末尾です。改行境界で切っても、コードフェンスの内側で切れることはあります。

### 4.5 403時の提案（G5）

静的経路の応答が403のとき、エラーメッセージを`HTTP 403 retryable=false hint=--render`にします。stderr先頭行は`webgrab: error=http HTTP 403 retryable=false hint=--render`となり、`error=<token> <message>`の書式は変わりません。終了コードは4のままです。403以外のステータスのメッセージは変えません。メッセージの生成は純関数`http_error_message(status)`に切り出します。

`hint=`は、終了コード6では`error=`行のトークンとして、終了コード4ではメッセージの末尾として現れます。どちらも先頭行にあるため、消費者は先頭行全体から`hint=`を探します。

`--auto-render`を指定していても、403ではエスカレーションしません。認証やIP遮断による403は`--render`でも解消しないため、この提案は成功を保証するものではありません。

### 4.6 文書とサンプルの更新

- 04-design.md: §3の「文字」の定義にある半開区間の式、§5の`--max-chars`行・`--auto-render`行（`text/plain`を判定対象外とする例外）・short-contentの段落、§6の`URL Source`・切り詰めの説明・jsonの`static_chars`の定義・text/htmlの節、§7のHTTP 4xx/5xx行・`text/plain`行・本文抽出0文字の行、§8の単体テスト一覧を本書の仕様に合わせます。
- 08-js-render-design.md: §4.2の手順6にある評価式と、§4.3の手順3にあるURLの規定を本書§4.2に合わせます。§4.3の手順2と§4.4の`--auto-render`行には、`text/plain`を判定対象外とする例外を追記します。§4.4の`static_chars`の定義には、`text/plain`では本文の文字数になることを追記します。
- `README.md`: curlとの比較表と使い方に、`text/plain`の素通しと改行境界での切り詰めを追記します。「セキュリティと信頼モデル」には、`text/plain`の本文に含まれる生のHTMLタグは無害化しないことを追記します。
- `src/cli.rs`: `--max-chars`のヘルプに、改行境界で短くなりうることを追記します。
- `samples/skills/`配下の3文書: 「使い分け」に、403時の`hint=--render`と、`text/plain`がそのまま返ること（空でも終了コード0）を追記します。

## 5. モジュール変更

| ファイル | 変更 |
|---|---|
| `src/fetch.rs` | `Accept`既定ヘッダの定数と設定。`http_error_message(status)`。`is_plain_text(content_type)` |
| `src/decode.rs` | HTML metaの走査を省くかどうかを引数で受け取る |
| `src/convert.rs` | `sanitize_link_schemes`を§4.1の3形に対応させ、`pipeline`から呼べる可視性にする |
| `src/pipeline.rs` | `text/plain`のとき`plain_stage(text)`で`Stage`を作る分岐。`Stage`に抽出を行ったかどうかの情報を持たせ、終了コード6・short-content・エスカレーションの免除に使う。render後URLの引き回し。CRAP値を30未満にするため、`run`を静的フェーズ、エスカレーション、出力の組み立てに分割する |
| `src/render.rs` | 戻り値を`Rendered { html, final_url }`に変更し、`drive`・`drive_inner`・`finalize`の戻り値型と既存の単体テストを合わせる。`resolve_final_url(requested, reported)` |
| `src/render/world.rs` | `dom_html`の評価式を`[location.href.slice(0, 8193), DOM HTML]`に変更し、両方を返す |
| `src/budget.rs` | `slice`の終端を§4.4の規則で決める |
| `src/cli.rs` | `--max-chars`のヘルプ文言 |
| `tests/common/mod.rs` | `Route`に任意のステータスを持たせる（追加ヘッダは既存の`headers`で`Location`を渡す）。E16〜E20用のfixtureを追加する |
| `tests/integration.rs` | `spawn_server`のresponderに、パスだけでなくリクエスト全文を渡す |

## 6. テスト戦略

### 単体（Chrome不要）

- `budget::slice`: 後半に改行がある、前半にしか改行がない、改行がない、最終ページ、`--max-chars 0`、`--max-chars 1`、マルチバイト文字、CRLF、終端が既に改行の直後、の各場合。加えて、継続を繰り返した連結結果が元の本文と一致し、各回の終端が`start`より大きいこと
- `fetch::http_error_message`: 403、404、429、500
- `fetch::is_plain_text`: `text/plain`、大文字混じり、`charset`付き、`text/html`、値なし
- `decode::decode`: `text/plain`では本文中の`<meta charset="shift_jis">`を採用せず、HTMLでは従来どおり採用すること
- `render::resolve_final_url`: http、https、値なし、`about:blank`、`chrome-error://`、`javascript:`、解釈できない文字列、改行を含む文字列、userinfoを含むURL（取り除かれる）、解釈後が8192バイトちょうど、8193バイト
- `convert::sanitize_link_schemes`: インラインリンク、区切りの後に空白があるインラインリンク、オートリンク、参照定義、大文字混じりのスキーム。加えて、通常のURL、`Mutex<T>`、生のHTMLタグ（`<a href="javascript:x">`）が変わらないこと
- `pipeline::plain_stage`: 本文がそのまま入り、`title`が値なしで、`](javascript:`が無害化されること

### 統合（Chrome不要、`tests/integration.rs`）

- I1: `text/plain`の応答で改行と`<T>`が保たれる
- I2: `text/plain`の199文字の応答と0文字の応答で、short-contentマーカーも`error=empty`も出ず終了コード0になる。199文字の応答を`--format json`で取得すると、`static_chars`が199で`rendered_chars`がnullになる。0文字の応答を`--format text`で取得すると、stdoutに本文もマーカーも出ない
- I3: `text/plain`の応答に`--auto-render`を付けても`info=auto-render`が出ず、`render_status`が`static`になる
- I4: `text/plain`の本文中の`[webgrab:truncated`と`](javascript:`が無害化される
- I5: `/a`が`/b`へ302で転送する構成で、`/a`と`/b`の要求の`Accept`が§4.3の値であり、`/robots.txt`の要求の`Accept`が`*/*`である。robots.txtはホップごとに取得されるため、接続はrobots、`/a`、robots、`/b`の4本になる
- I6: 403で終了コード4かつstderr先頭行に`hint=--render`があり、`--auto-render`を付けても`info=auto-render`が出ない。404では`hint=`がない
- I7: 各行が`--max-chars`の半分より短く、`[webgrab:`、制御文字、危険リンクスキームを含まない`text/plain`の本文を、小さい`--max-chars`と`--format text`で取得する。各ページの出力からwebgrabが付けた部分（フッタ行と出力末尾の改行）を除くと、最終ページを除く各ページの本文が改行で終わり、各ページの本文の連結が全文と一致する
- I8: 同じ`text/plain`のURLを既定、`--raw`、`--format text`、`--format html`で取得した本文が一致する

### E2E（実Chrome、`tests/render_e2e.rs`）

- E16: `/old`が`/dir/page`へ302で転送し、転送先に相対リンク`next.html`がある。`--render`の`URL Source`が`/dir/page`で、リンクが`/dir/next.html`に解決される
- E17: 短いプレースホルダを返し、読み込み時に同期実行されるインラインスクリプトの`location.replace`で`/dir/page`へ移るページ。`--auto-render --format json`の`url`が`/dir/page`で、小さい`--max-chars`を指定したときの`continue_command`のURLも`/dir/page`になる
- E18: 描画後に`history.pushState`でパスを変えるページ。`URL Source`が変更後のパスになり、オリジンは変わらない
- E19: テストサーバを2つ起動し、ポートAのページが同期実行されるインラインスクリプトの`location.replace`でポートBの`/dir/page`へ移る（ポートが違えばオリジンが違う）。`--render --allow-private`の`URL Source`のポートがBになる
- E20: 静的本文が200文字未満で、スクリプトが本文をさらに短く置き換えたうえで`history.pushState`によりパスを変えるfixtureを追加する。`--auto-render --format json`の`render_status`が`no-gain`で、`url`が変更前のパスのままである

## 7. 実動作検証（commit前）

項目ごとに、releaseビルドのバイナリで次を確認し、結果を[07-verification-report.md](07-verification-report.md)に記録します。

| # | コマンド | 合格条件 |
|---|---|---|
| V1 | `webgrab https://raw.githubusercontent.com/tokio-rs/tokio/master/tokio/src/sync/mutex.rs --max-chars 10000000 --no-tokens` | 本文の行数、`Mutex<T>`の個数、`<T: ?Sized>`の個数がcurlの取得結果と一致する |
| V2 | `webgrab https://docs.rs/tokio --render --max-chars 200000` | `URL Source`が`https://docs.rs/tokio/latest/tokio/`で、本文に出現する順で重複を除いたdocs.rsリンクの先頭6本がHTTP 200を返す |
| V3 | `webgrab https://crates.io/crates/tokio --auto-render --max-chars 0` | 終了コード0 |
| V4 | `webgrab https://doc.rust-lang.org/book/ch03-02-data-types.html --max-chars N`（Nは1500から900刻みで21通り） | 行の途中で切れた回数が変更前の18回より減り、残った各回は切断位置の行が`N`の半分を超える長さである。Nを1つ選び、`--format text`で継続コマンドを最後まで実行する。本文に`[webgrab:`と制御文字が現れないことを確かめたうえで、各ページの出力からwebgrabが付けた部分（フッタ行と出力末尾の改行）を除いた本文の連結が、`--format text --max-chars 10000000`の本文と一致する |
| V5 | `webgrab https://stackoverflow.com/questions/27535289/what-is-the-correct-way-to-return-an-iterator` | 終了コード4で、stderr先頭行に`hint=--render`がある |

外部サイトの応答が変わって再現できない項目は、統合テストまたはE2Eの同等ケースで代替し、その旨を記録します。

## 8. 完了条件（機械検証可能）

1. `cargo test --lib --bins --test integration`が終了コード0
2. `WEBGRAB_E2E=1 cargo test --test render_e2e -- --test-threads=1`が終了コード0
3. `cargo fmt --check`と`cargo clippy --all-targets -- -D warnings`が終了コード0
4. `python3 tools/doclint.py docs/`が`Critical 0 / High 0`
5. `cargo llvm-cov`の`--fail-under-lines 80`が終了コード0
6. `WEBGRAB_E2E=1 cargo llvm-cov --lib --bins --test integration --test render_e2e --lcov --output-path lcov.info -- --test-threads=1`の後に`cargo crap --lcov lcov.info --min 30`を実行する。`--min 30`はCRAP値が30以上の関数だけを表示する。その出力を`grep`で調べ、`src/pipeline.rs`、`src/fetch.rs`、`src/budget.rs`、`src/decode.rs`、`src/convert.rs`の関数と、`src/render.rs`・`src/render/world.rs`にある`render`、`render_inner`、`drive`、`drive_inner`、`finalize`、`resolve_final_url`、`dom_html`が現れない
7. 変更をcommitした後に`git diff master...HEAD > mutants.diff`と`cargo mutants --in-diff mutants.diff`を実行し、生存したミュータントがない。等価なミュータントが残る場合は、理由を07-verification-report.mdに記録する
8. §6のI1〜I8とE16〜E20が存在し、合格する
9. §7のV1〜V5の結果が07-verification-report.mdに記録されている
10. PRのCIがcheck、test、coverage、securityの各ジョブで緑

## 9. やらないこと（再掲）

`text/plain`以外の非HTML形式の受け入れ、HTTPエラー時の自動エスカレーション、コードフェンスを認識した切り詰め、`Accept-Language`の送出、Content-Type欠落時の内容推定は行いません。
