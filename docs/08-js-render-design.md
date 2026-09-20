# JS描画ページ取得改善 設計書

- バージョン: 1.6（seal。v1.5に実装前probeの補遺（分離ワールドの寿命、`page.content()`のメインワールド依存）を反映。敵対的レビューRound 1〜4の所見を一次情報で検証して反映。Round 4はユーザー承認で延長した最終ラウンドで、その所見の反映は本版で完了。変更履歴は末尾）
- 日付: 2026-08-27
- 対象: webgrab 0.1.0（[04-design.md](04-design.md) v1.2 sealをベースにした差分設計。実装時に04をv1.3へ更新する）
- 根拠: 本書§1の再現ログ（本セッションで実測）、chromiumoxide 0.9.1のソース（`src/page.rs`, `src/handler/network.rs`, `src/handler/target.rs`, `src/detection.rs`）、dom_smoothie 0.18.0のソース（`src/lib.rs`の`ReadabilityError`）、GitHub Actions runner-images（Ubuntu 24.04にGoogle Chrome 151同梱）、CDPイベントのprobe実測（§1末尾）

## 1. 目的と観測事実

ユーザー報告は「JavaScriptで描画されたページの文字が読み取れない」である。`--render`（chromiumoxide + Chrome）は既に実装済みで動作する。原因は、既定経路がJS描画ページを検知して`--render`へ誘導できないことにある。再現した故障は次の2件である。

| # | 再現手順 | 観測 | 原因 |
|---|---|---|---|
| F1 | 本文をJSで後から挿入する空シェルのHTMLを既定（静的）で取得 | `webgrab: error=internal readability parse failed` / `failed to grab the article`、終了コード1 | dom_smoothieが本文なしで`Err(GrabFailed)`を返す。04-design.md §7の「本文抽出0文字 → 終了コード6 + `--render`提案」は`Ok(空)`前提で、`Err`経路では発火しない。エージェント（Claude Code / Codex / Kimi）は`--render`へ切り替える手掛かりを得られない |
| F2 | 描画完了に4秒かかるページを`--render`で取得 | 本文が「読み込み中...」の8文字だけ（終了コード0 + short-contentマーカー） | `render.rs`は`goto`後に固定`--wait-ms`(2000ms)をsleepするだけで、load完了・ネットワーク静止・DOM安定のいずれも待たない。`goto`は`Page.navigate`の応答で戻り、load完了を待たない（chromiumoxide `src/page.rs` `goto`） |

補足事実として、`--render`単体はreact.dev / qiita.com / ローカルCSR fixture（500ms遅延）で正常に本文を返す。静的取得からrenderへの自動切替は存在しない（04-design.md §3「レンダリング既定」）。

§4.2の前提は、使い捨てのprobe（chromiumoxide 0.9.1 + ローカルHTTPサーバ、2026-08-27）で次のとおり実測した。(1) `Fetch.failRequest`した要求にも`Network.loadingFailed`（`net::ERR_ACCESS_DENIED`）が届く。(2) リダイレクトは同一`RequestId`で`requestWillBeSent`が再送され（`redirect_response`あり）、完了通知は1回。(3) `data:` URLにも`requestWillBeSent`と`loadingFinished`が届く。(4) `Fetch.requestPaused`の`request_id`は`interception-job-N`形式で`Network`の`RequestId`とは別空間であり、対応づけには`network_id`を使う。(5) `goto`直後の`wait_for_navigation`は即時に返る。(6) ページ内で長さだけを計算する評価式は数値配列で戻る。また、F2の再現markup `<div id="app">読み込み中...</div>`はdom_smoothieが本文8文字として抽出する（`Ok`）一方、`<div id="app"></div>`は`Err(GrabFailed)`になることを確認した。

Round 3所見の検証として2回目のprobeで次を実測した。(7) メイン文書を`Fetch.failRequest`すると`goto`は`Err(net::ERR_ACCESS_DENIED)`を返す。iframeの遮断では`goto`は`Ok`。(8) iframeの文書要求は`resource_type=Document`だが`frame_id`がメインフレーム（`page.mainframe()`で取得）と異なる。(9) `Network.dataReceived`の`data_length`は展開後バイト（`Fetch.enable`（Request stage）+ 全要求`continueRequest`の状態で、300,076バイトの非圧縮本文に対し300,076、`loadingFinished.encodedDataLength`は300,163）であり、静的経路の`read_capped`（reqwestの`chunk()`＝展開後）と同じ単位で計上できる。圧縮時の意味はCDP定義（`Network.pdl`の`dataLength`「Data chunk length」/`encodedDataLength`「might be less than dataLength for compressed encodings」、chromiumoxide_cdp `src/cdp.rs`の同フィールドdoc）が一次情報で、E13が実測確認を担う。

3回目のprobe（別サイトiframe、`data:`/`blob:` iframe）で次を実測した。(10) `data:`と`blob:`のiframe文書は`dataReceived`に展開後長で計上される（200,007 / 150,007バイト）。(11) 別サイト（`localhost`）iframeは文書要求の`requestPaused`こそ親セッションに届くが、その子フレーム内のサブリソース要求と`dataReceived`は親セッションに現れない（OOPIFは別セッション）。Chromeを`--disable-features=site-per-process,IsolateOrigins`で起動しても本セッションでは改善を観測できなかった（未確認扱い）。(12) `page.mainframe()`は`about:blank`の時点（`goto`前）で`Some`を返し、`FrameId`は`PartialEq`で比較できる。

5回目のprobe（実装前の確定用）で次を実測した。(13) `about:blank`上で作った分離ワールドはナビゲーション後に破棄され、その`context_id`での評価は`Cannot find context with specified id`になる。分離ワールドは`goto`後に作り、この失敗時は1回だけ作り直す。同じフレーム・同じ`world_name`での再作成は同じ`context_id`を返す。(14) ページ側が`document.documentElement.outerHTML`のgetterを上書きすると、chromiumoxideの`page.content()`（メインワールドで`outerHTML`を評価する実装、`src/page.rs:1332`）は偽の値（3バイト）を返す。分離ワールドで同じ式を評価すると正しい長さ（518バイト）が返る。したがってDOM HTMLの取得も分離ワールドで行う。(15) `tokio::sync`（`Semaphore`/`Mutex`）は依存経由で有効化済みでCargo.tomlの変更は不要。

## 2. スコープ

- 対象: (a) F1のエラーマッピング修正、(b) F2の待機戦略改善、(c) opt-inの自動エスカレーション`--auto-render`、(d) 実Chromeを使うE2Eテストと、それを回すCI（GitHub Actions）、(e) エージェント向けSKILL文書の更新
- 対象外: 既定挙動で常にChromeを起動すること（04-design.md §3の「静的既定」を維持）、`--render`明示時のChrome未検出における静的取得へのフォールバック（04-design.md §3の不採用決定を維持。`--auto-render`時の扱いは§4.3で別途定義し、04 §3にその例外を明記する）、中央リポジトリ`okamyuji/reusable-workflows`へのrust用workflow追加、ブラウザ指紋の偽装、Cookie/ログイン状態の利用、OOPIF/Service Workerの子セッションへの`Fetch`/`Network`付与

## 3. 決定表（MECE、全行確定済み）

| 決定 | 選択肢 | 採用 | 理由 |
|---|---|---|---|
| F1の扱い | 内部エラーのまま / 終了コード6へ写像 / 静的経路で自動render | `GrabFailed`のみ終了コード6へ写像 | dom_smoothieの`ReadabilityError::GrabFailed`は「本文が見つからない」と同義で、既存契約（§7 本文抽出0文字）に一致する。他の変種（`TooManyElements`等）は資源上限であり内部エラーのまま残す |
| エスカレーション方式 | A: ヒント改善のみ / B: opt-in `--auto-render` / C: 既定で自動 | B | 既定挙動（速度・依存の軽さ）を変えず、フラグ指定時はエージェントの再呼び出しを不要にする。切替と失敗はstderrとJSON/frontmatterの`render_status`で明示する。Cは一覧ページ等で毎回Chromeが起動しコストが増える |
| エスカレーション判定 | 空本文のみ / 空または短文（200文字未満） / SPAシグナル検知 | 可視テキスト（`convert::visible_text_len`。タグ・script・style・noscriptを除去し空白を畳んだ文字数。リンク先や画像URLは含めない。失敗しない純関数）が空または200文字未満。スライス前の全文で判定し、`--format`と`--raw`に依存しない | 既存のshort-content閾値（04-design.md §5、200文字）と同じ数値を使う。`convert::to_text`はhtmd経由でリンク先URLを残すため、ナビ付き空シェルで200文字を超えて発火しない。Markdown長やHTML長で測ると`--raw`や`--format html`でタグ分だけ長くなり発火しない。既存のshort-content判定と終了コード6判定は従来どおり変換後本文の長さで行い、エスカレーション判定だけが可視テキスト基準となる非対称は§4.3に明記する。`--start-index`/`--max-chars`の影響を受けないため続き取得でChromeが再起動しない。SPAシグナル検知は誤検知と保守コストが高い |
| `--auto-render`でrender失敗時 | 全失敗を伝播 / 全失敗で静的結果へ復帰 / 安全事象のみ伝播 | 安全事象のみ伝播。renderフェーズの終了コード7（Chrome起動・CDP・タイムアウト）、renderフェーズの`--max-bytes`超過、render後のextract/convert失敗は`warn=auto-render-failed reason=<render\|max-bytes\|extract>`を出して静的結果へ復帰する。終了コード8（メインフレームのナビゲーションが内部アドレスへ到達）だけは出力なしで伝播し、renderフェーズの他のいかなる失敗分類よりも優先して判定する | 静的で得た結果を捨てない。`--max-bytes`超過と抽出失敗は「このページはrenderに向かない」という事実であり、静的結果を捨てる理由にならない。内部アドレス到達はSSRF試行の検知であり、既存の終了コード8の契約（出力なしで中断）を維持して通知の確実性を優先する。`--render`明示時の終了コード7/4は従来どおり |
| render経路の`--max-bytes`計上 | プロキシのワイヤバイトのみ（現状） / `Network.dataReceived.data_length`（展開後）の合計 / 両方 | 両方。主判定は`dataReceived.data_length`の合計（ページセッションの全要求の展開後バイト。`data:`/`blob:`を含む）で、`--max-bytes`（既定20MiB）を超えた時点でrenderを打ち切り終了コード4（`--auto-render`時は`reason=max-bytes`で静的へ復帰）。加えて`page.content()`の直前にDOMのシリアライズ長を1回評価し、残余上限を超えていれば同じ扱いにする。プロキシのワイヤバイト計上は同じ上限で第二の保険として残す | `--max-bytes`の契約は「展開後バイト」（04 §5）であり、ワイヤバイトだけではgzip爆弾（最大約1032倍）で展開後が上限されない。`dataReceived`はChromeの復号後長を返す（§1 probe(9)(10)）。ネットワークを経ずJSで膨らませたDOMは`dataReceived`に現れないため、`content()`前のDOM長評価で塞ぐ（chromiumoxideのWebSocketは`max_message_size(None)`で転送側に上限が無い）。残余: 別セッションのOOPIF/Service Worker要求は展開後計上から漏れ、プロキシのワイヤ計上（同じ上限）でしか有界にならない。その内容は`page.content()`（メイン文書のみをシリアライズ）に含まれないため、超過しうるのはChromeレンダラのメモリであってwebgrabの出力・メモリではない。定量では既定20MiBのワイヤに対し最悪約20GBの展開後がChrome側で生じうる。子セッションへの`Fetch`/`Network`付与は次版候補（§10） |
| render結果の採否 | 無条件採用 / 静的と比較して長い方 | 長い方を採用。静的を採用した場合は`render_status=no-gain reason=shorter` | JSチャレンジやヘッドレス検知でrender結果が静的より短くなりうる。静的結果の破棄はデータ喪失 |
| 待機戦略 | 固定sleep / ネットワーク静止のみ / DOM安定のみ / 複合条件+上限 | 複合条件+上限 | ネットワーク静止だけではタイマー描画（F2）を、DOM安定だけでは「読み込み中」の一時安定を誤検知する。「ネットワーク静止 かつ DOM安定 かつ 可視テキスト200文字以上」を満たしたら早期終了し、満たさなければ`--wait-ms`上限まで待つ。限界は§4.2に明記 |
| skip閾値 | 固定値 / 実測に基づく値 | 暫定で残余5秒・256KiB。§8でChromeコールドスタートLを実測し、閾値が「L + `content()`予備2000ms + 有意な待機1000ms」を下回るなら引き上げる | Chrome起動が閾値を超えると、起動して必ず終了コード7で静的へ戻る無駄が生じる。renderの予算はChrome起動前から消費されるため、起動時間を勘定に入れる |
| `--wait-ms`の意味 | 追加待機（現状） / 上限 | 上限（既定5000ms）。起点は`goto`直前 | 早期終了で通常ページは速くなり、遅いページには余裕を与える。フラグ名は変えず既定値と説明を変更する |
| in-flight計数 | 符号付き加減算 / `RequestId`集合 | `RequestId`集合（`requestWillBeSent`で挿入、`loadingFinished`/`loadingFailed`で削除。`redirect_response`付きの`requestWillBeSent`は同一IDの再送なので集合上は冪等）。削除済みIDは短命のtombstoneに残し、順序が入れ替わって後から届いた挿入を無視する | 加減算はリダイレクト（同一IDで複数回`requestWillBeSent`）で正に張り付き、取りこぼしで負になる。集合は冪等で、`Fetch.failRequest`した要求はintercept側でも`network_id`をキーに削除する（§1のprobeで`loadingFailed`が届くことも確認済みなので二重の保険） |
| in-flight計数の情報源 | CDP `Network`イベント / `Fetch.requestPaused`のみ | `Network.requestWillBeSent` / `loadingFinished` / `loadingFailed` | chromiumoxideはPage初期化で`Network.enable`を発行済み（`src/handler/network.rs`）。`Fetch`は完了を通知しない |
| DOM安定の計測 | `page.content()`全文比較 / JSで長さを取得 | 要素数（`getElementsByTagName('*').length`）と`innerText`長の2数値をページ内で計算し、`Page.createIsolatedWorld`で作った分離ワールドの`context_id`を指定した`evaluate_expression`で受け取る | 転送量が一定で、Rust側に巨大文字列を持ち込まない。`outerHTML`の全体シリアライズは毎回O(DOMサイズ)なので採らない（`content()`直前の1回のみ）。分離ワールドで評価するためページ側の`defineProperty`等による上書きが効かない。`innerText`はレイアウト計算を伴うがポーリング回数は`--wait-ms`で有界 |
| E2Eの対象 | 実在サイト / ローカルfixture | ローカルfixture（`tests/`内の最小HTTPサーバ + 実Chrome）。gzip fixtureは`tests/fixtures/big_gzip.html.gz`としてコミットし`include_bytes!`で読む | 外部サイト依存はCIでflakyになる。fixtureはF1/F2の再現物そのもの。圧縮crateをdev-dependencyに足すよりチェックインの方が依存が増えない（R7） |
| E2Eの有効化 | 常時 / 環境変数opt-in | `WEBGRAB_E2E=1`で有効。未設定かつ`CI`未設定ならstderrにskip理由を出して合格扱い。未設定かつ`CI`設定済みなら失敗 | ローカルの`cargo test`をChrome依存にしない。CIで環境変数が落ちても静かに緑にならない |
| CIの置き場所 | 中央reusable-workflows / リポジトリ内 | リポジトリ内`.github/workflows/ci.yml` + `security-scan.yml@v1` | 中央にrust用workflowが存在せず、ChromeセットアップとE2Eがこのプロジェクト固有。中央のtag張り替え手順を本作業に持ち込まない。逸脱として§7に記録する |
| Chromeの取得（CI） | setup-chromeアクション / runner同梱 | runner同梱（`ubuntu-24.04`に固定。Google Chrome 151）。`ubuntu-latest`は26.04へ移行予定でChrome同梱とsandbox挙動が同時に変わるため使わない | 追加ステップ不要。chromiumoxideの既定検出は`CHROME`環境変数を最優先し、次にPATHの`chrome` / `google-chrome-stable` / `chromium` / `chromium-browser`等を探す（`src/detection.rs`） |
| Chrome sandbox | 常に有効 / 環境変数で無効化 / CLIフラグで無効化 | CLIフラグ`--no-sandbox`（既定off）。有効時は`warn=no-sandbox`を出し、継続コマンドに再現する（SKILLに「継続コマンドに`--no-sandbox`が含まれうる。必要だった環境の外で実行しない」と明記）。CIのtestジョブは最初からこのフラグをE2Eハーネス経由で付ける | sandboxは敵対HTMLを描画する唯一の封じ込め層なので、無効化は`--help`に見える明示フラグに限る。環境変数だけで落とせる隠れトグルは作らない。Ubuntu 24.04ランナーはunprivileged user namespaceの制限でsandboxが起動しない事例があり、使い捨てVMかつ対象がローカルfixtureのCIでは無効化を許容する |
| 継続コマンドの再現 | `--auto-render`をそのまま再現 / 経路に応じて置換 | エスカレーション時は`--render`へ置換、非エスカレーション時は`--auto-render`を省略。`--chrome-path`と`--no-sandbox`も再現する（`--chrome-path`は既に再現済み） | 続き取得は初回と同じ経路を確定的に再現する。非エスカレーション時に`--auto-render`を残すと、続きの呼び出しで静的応答が短かった場合に別の文書を切り出す |
| 経路の表示 | 追加しない / 真偽値 / 状態列挙 | `render_status`（`static` / `rendered` / `failed` / `no-gain` / `skipped`）をJSONとfrontmatterに追加。markdown / text / htmlでは`failed` / `no-gain` / `skipped`のときだけ自己記述行`[webgrab:render-status <status> reason=<token>]`を本文末尾に付ける | 真偽値では「試みていない」「失敗した」「採用しなかった」を区別できない。既定形式でもstdoutだけで異常が分かるようにする。正常な`static` / `rendered`では行を増やさずMarkdownヘッダ（Jina互換）も変えない |
| カバレッジ閾値 | 60 / 80 | `--fail-under-lines 80`。coverageジョブはE2E（実Chrome）を含めて計測し、`render.rs`の除外を外す | 現状88.6%（本セッション実測、render.rs除外）。本改訂の中心であるrender.rsとpipelineのrenderフェーズを計測対象に入れないと、閾値が新規コードを見ない。実測で80を割った場合は除外を戻し差分をバックログに記録する |

## 4. 仕様

### 4.1 静的経路のエラーマッピング（F1）

`extract::extract`はdom_smoothieの`parse`が`Err(ReadabilityError::GrabFailed)`を返した場合、stderrに`webgrab: warn=extract-grab-failed`を1行出し、本文HTML空文字列の`Extracted`を返す。その結果pipelineの空本文チェックが働き、終了コード6となる。終了コード6のエラーブロック先頭行は`webgrab: error=empty hint=<提案>`とし（short-content警告と対称の`hint=`トークンを追加）、2行目に`empty body extracted; retry with <提案>`を出す。提案は`render_status`で決め、`static` / `skipped`なら`--render/--raw`、`rendered` / `failed` / `no-gain`なら`--raw`とする（short-content提案と同じ規則。`--render`明示時も`rendered`扱い）。`GrabFailed`以外の`parse`失敗と`Readability::new`の失敗は従来どおり内部エラー（終了コード1）のままとする。トークン付きのエラー（現状は終了コード6のみ）は先頭行に人間可読メッセージを含めず、メッセージは2行目の先頭に出す。他の終了コードの`webgrab: error=<token> <message>`書式は変えない（この書式差は04 §5に記載する）。

「先頭行」の定義: 04-design.md §5の「stderrのエラー・警告の先頭行は機械可読の固定書式」は、各メッセージブロック（`webgrab: `で始まる行とそれに続く詳細行）の1行目を指す。プロセスのstderr全体の1行目ではない（`warn=`や`info=`がエラーに先行してよい）。テストの期待は「その書式で始まる行が含まれる」で書く。

### 4.2 待機戦略（F2）

`render::drive`の`goto`以降を次の手順に置き換える。`render()`は開始時に`deadline = now + RenderOptions.timeout`を確定し、Chrome起動・プロキシ起動・`drive`のすべてがこの期限の内側で動く。待機の起点は`goto`直前、上限は`--wait-ms`（既定5000ms）を`deadline`で丸めた実効上限（手順5）とする。手順0として、`Fetch.enable`の前に`page.mainframe()`でメインフレームIDを取得し（§1 probe(12)）、`Err`または`None`なら終了コード7で中止する（fail-closed）。分離ワールド（`Page.createIsolatedWorld`、メインフレーム、`world_name=webgrab`）は`goto`の後に作り（`about:blank`時点の文脈はナビゲーションで破棄される。§1 probe(13)）、以後の全evaluateにその`context_id`を指定する。`Cannot find context`で失敗したら1回だけ作り直す。

1. `goto`前に`Network.requestWillBeSent` / `loadingFinished` / `loadingFailed` / `dataReceived`を購読する監視タスクを起動し、`RequestId`集合`in_flight`を更新するとともに、`dataReceived.data_length`を`decoded_total`に加算して`RenderOptions.max_bytes`を超えたら`decoded_exceeded`を立てる（監視タスクが超過を検知した時点で`decoded_exceeded`を立て、ポーリング側は次の確認で終了コード4に向かう。超過分は最大1ポーリング間隔（250ms）の受信量。`--render`明示時は04 §5どおり終了コード4、`--auto-render`時は§4.3 5の`reason=max-bytes`）。`in_flight`と`decoded_total`は`Arc<Mutex<_>>`で監視タスク・interceptタスク・ポーリングの三者が共有し、`Network`の4イベントは種別ごとに`event_listener`を1本ずつ張って監視タスクが`select!`で読む（挿入・削除とも冪等。削除済みIDは短命のtombstoneに残し、後から届いた同一IDの挿入を無視する）。interceptタスクが`Fetch.failRequest`を発行した要求は、`EventRequestPaused.network_id`が`Some`のときその値をキーに集合から削除する（`None`なら何もしない）。interceptは受信ループから各イベントを個別タスク（同時実行16まで、超過分はキュー）に渡して処理し、ホスト名の判定結果は実行単位のキャッシュ（`host:port → 判定`。プロキシ側の判定と共有）に保存する。これにより1件の遅い解決が他の要求（特にメインナビゲーション）の処理を塞がない。両タスクのハンドルはDropでabortするガードで保持し、早期リターン・`--timeout`キャンセルのどの経路でも残置しない。
2. `goto`の後、`wait_for_navigation`（load完了）を`min(1000ms, 残り時間)`を上限に`tokio::time::timeout`で待つ。既にloaded扱いなら即時に返り、上限に達したらそのまま次へ進む（load未発火のページで待機予算を使い切らないため）。この手順は最善努力であり、正しさは手順3の条件だけに依存する。`goto`自体が`Err`を返した場合は、手順6と同じintercept同期待ち（最大500ms）と`main_blocked`再確認を行ってから終了コードを決める（立っていれば8、立っていなければ7）。
3. 250msごとに次を評価する。
   - `in_flight`が空
   - ページ内で計算した`[要素数, innerText長]`の2数値が直前の観測と等しい（直前との一致が2回連続なら「安定」。観測3回が必要なので最短500ms。初回の観測はナビゲーション待ちの直後に行い、ポーリング間隔を挟まない）
   - `innerText`のtrim後の文字数が200以上（短文閾値と同じ定数を共有する）
   すべて満たしたら終了する。評価式は`(function(){var b=document.body;return [document.getElementsByTagName('*').length,(b&&b.innerText||'').trim().length];})()`のように、常に数値配列を返し、`body`が無い文書でも例外を投げない形にし、分離ワールドの`context_id`を付けた`Page::evaluate_expression`（`EvaluateParams`）で送る。evaluateの失敗・タイムアウト・数値以外の戻り値は「条件未達」として扱い、終了コード7にはしない。各evaluateは残り時間を上限にタイムアウトさせる。
4. 各ポーリングの前後と手順3の終了後に`main_blocked`を再確認し、立っていれば終了コード8で中断する。`main_blocked`は`resource_type == Document`かつ`frame_id`がメインフレームIDに一致する要求（＝メインナビゲーション）が内部アドレス宛だったときに限って立てる。サブフレーム（iframe）やサブリソースの内部アドレス要求は`Fetch.failRequest`で遮断するだけで`main_blocked`を立てない（1個のiframeで終了コード8を強制されないため）。判定は純関数`is_main_navigation(resource_type, frame_id, main_frame_id)`とし単体テストする。
5. 経過時間が実効上限に達したら、条件未達でも終了する。実効上限は`goto`直前に`min(--wait-ms, deadline − now − 2000ms)`（`saturating_sub`、負なら0）として確定し、待機上限到達は正常終了として外側のタイムアウト（終了コード7）に先んじる。2000msは手順6（同期待ち500ms + DOM長評価 + `content()`）の予備。
6. interceptが受け取った`requestPaused`の個別タスクがすべて完了するのを最大500ms待ってから（受信+1 / 完了+1のカウンタ一致。処理中のイベントの完了だけを保証し、未受信のイベントは対象外）、手順4の再確認を再度行う。次に分離ワールドで`document.documentElement.outerHTML.length`を1回評価し（この1回だけO(DOMサイズ)）、`RenderOptions.max_bytes − decoded_total`を超えていれば終了コード4（`--auto-render`時は`reason=max-bytes`）で中断する（JSでネットワークを経ずに膨らませたDOMを`content()`前に塞ぐ。文字数はUTF-8バイト数の下界なので安全側）。最後に、同じ分離ワールドの評価で`[location.href.slice(0, 8193), doctype + document.documentElement.outerHTML]`の配列を取得して返す（10-fetch-fidelity-design.md §4.2）。この評価の上限は`min(残り時間, 実効wait残余 + 2000ms)`とし、超過は終了コード7にする。評価自体が失敗した場合と、配列の要素1（DOM HTML）が文字列でない場合も終了コード7とする。要素0（render後URLの候補）は純関数`resolve_final_url(requested, reported)`に渡し、採用の可否を決める。chromiumoxideの`page.content()`はメインワールドで評価するためページ側のgetter上書きで偽装でき（§1 probe(14)）、使わない。
7. 終了コード8の判定は単一の経路で行う。`render_inner`は`drive`の戻り値（`Ok`/どの`Err`か）によらず、`drive`完了後にまず`main_blocked`を読み、立っていれば終了コード8を返す（`goto`失敗、evaluate/`content()`タイムアウト、展開後超過、DOM長超過、プロキシ`exceeded()`のいずれで戻った場合も同じ）。プロキシ側で遮断した要求はカウントし、1件以上あればstderrに`webgrab: warn=netguard-blocked layer=proxy count=N`を1行出す（interceptで遮断したサブリソースも`layer=intercept`で同様）。この2行は終了コード8で終わる実行でも`error=`ブロックの前に出す。これにより、メインナビゲーション以外のSSRF試行も無通知にはならない。終了コード8の詳細行は`layer=intercept host=<host> resolved=<ip> range=<range> (intercept=N proxy=M; use --allow-private to override)`とし、解決IPと拒否レンジを含める（04-design.md §7）。名前解決に失敗した場合は`resolved=unresolved`、2秒の解決上限を超えた場合は`resolved=timeout`とし、いずれも`range=`を省く。静的経路（fetch.rs）の終了コード8詳細行も`host=... resolved=... range=...`で同じ書式にそろえる。

判定と計数は`src/render/wait.rs`に純関数として切り出し、Chromeなしで単体テストする。`InFlight::on_request(id, is_redirect)` / `on_done(id)` / `is_idle()`、および`should_stop(idle, stable_polls, text_len, elapsed, cap) -> bool`。

interceptハンドラとrenderproxyのホスト名解決には、それぞれ上限（2秒）を設け、超過はfail-closed（遮断）とする。判定結果は実行単位のキャッシュで両層が共有し、同一ホストの二重解決を避ける。`spawn_blocking`上のgetaddrinfoはキャンセルできず上限超過後もスレッドが残るが、プロセス終了で回収される（既知の性質）。

このアルゴリズムの限界を明記する。(a) SSE / long-polling `fetch`のような完了しない要求を持つページは`in_flight`が空にならず上限まで待つ（WebSocketは`requestWillBeSent`を発火しないため影響しない）。(b) 本文が本来200文字未満のページも上限まで待つ。(c) ナビゲーションやサブリソースを伴わないタイマーのみで本文を描画し、かつシェル時点で可視テキストが200文字以上あるページ（ナビ・フッタが長いSPA）は、シェルの安定を「完了」と誤認して早期終了する。この場合は`--wait-ms`を大きくしても改善しない（上限であって最小待機ではない）ため、`--raw`や再実行で対処する。(d) クロスオリジン（別サイト）iframeとService Workerの要求は別セッションのため、第一層のFetch interceptionにも`dataReceived`計上にも現れない（§1 probe(11)。04 §3.1の「全サブリソース」は実装時に訂正する）。SSRFはプロキシ（第二層）が遮断し、バイト量はプロキシのワイヤ計上で有界にする。(a)(b)(d)は結果が正しく待ち時間が上限に張り付くだけである。(c)は本書の限界として文書化し、対処は改善バックログに記録する。(e) `main_blocked`はinterceptが非同期に立てるため、手順6の同期待ち（最大500ms）を超えて遅れた場合は終了コード8ではなく0で終わりうる。個別タスク化とキャッシュにより遅延は「メインナビゲーション1件の解決（最大2秒）」に縮むが、ゼロにはならない。遮断自体はFetch層で成立しているので漏洩は起きず、`warn=netguard-blocked`で通知は残る。(f) `EventRequestPaused.network_id`が`None`の要求（Service Worker経由等）は集合から削除できず、`loadingFailed`も届かなければ上限まで待つ。(g) tombstoneは`TOMBSTONE_MS`（2000ms）で失効し、`in_flight`とtombstoneは各4096件を上限に最古から捨てる。上限で最古を捨てた後は、待機上限まで「静止」と判定しない（捨てた要求が未完了でも早期終了しないため）。

### 4.3 `--auto-render`

フラグ`--auto-render`（既定off）。`--render`と同時指定された場合は`--render`が優先され、`--auto-render`は無視する（stderr注記なし）。処理は次のとおり。`pipeline::run`は「静的フェーズ → エスカレーション判定 → renderフェーズ → 出力」の順に再構成し、既存の空本文チェック（終了コード6）はエスカレーション判定の後に置く。

1. 静的経路（fetch、decode、extractまたは`--raw`の`strip_non_content`、convertの順）を従来どおり実行し、本文HTML、変換後本文（スライス前の全文）、`final_url`、消費バイト数、経過時間を得る。静的経路がエラー（終了コード3・4・5・8等）で終わった場合はエスカレーションせず、そのまま伝播する。
2. 本文HTMLの可視テキスト長（`convert::visible_text_len`。失敗しない）が0、または200文字未満なら（`--raw`でも同じ基準。`--format`には依存しない）、エスカレーションを試みる。text/plainはこの判定の対象外で、Chromeで描画しても同じテキストしか得られないため常に`render_status=static`のままとする（10-fetch-fidelity-design.md §3）。既存の終了コード6判定とshort-content判定は従来どおり変換後本文の長さで行うため、`--format html`等では「エスカレーションはしたがshort-contentマーカーは出ない」非対称がありうる（可視テキストは短いがHTML長は200以上の場合）。この非対称は既存契約を変えないための意図的なもので、経路は`render_status`と`[webgrab:render-status ...]`行で分かる。ただし残余予算（`--timeout` − 経過時間）が5秒未満（暫定。§3決定表）の場合はstderrに`webgrab: warn=auto-render-skipped reason=timeout`を出し、`render_status=skipped`として静的結果へ進む。残余`--max-bytes`（指定値 − 静的経路の展開後消費バイト数）が256KiB未満の場合も同様に`reason=max-bytes`でskipする。`--max-chars 0`でも判定とrenderは行う（総文字数の下見でもChromeが起動しうる）。
3. エスカレーション時はstderrに`webgrab: info=auto-render reason=<empty|short> chars=<N>`を1行出し、render経路を実行する。`RenderOptions.timeout`と`max_bytes`には残余予算を渡す（render側の超過メッセージは「remaining budget N of M」の形で利用者指定値も併記する）。`--max-bytes`は両経路とも展開後バイトで計上する（静的は`read_capped`、renderは`dataReceived.data_length`の合計とDOM長。§3決定表）。範囲は異なる。静的経路は1応答ごと（リダイレクト各段は個別）、render経路は実行全体の累計で、静的では通るがrenderで超過するページ（20MiB未満の応答が多数）がありうる。この差は意図的で、render経路の累計はChromeが取得する総量を有界にするためのもの。renderフェーズの抽出（extract/`strip_non_content`）は、採否によらず常にrender後URLを`base_url`（相対リンクの解決基準）として使う。`Meta.url`（`URL Source`）と継続コマンドのURLは、render結果を採用したとき（`render_status=rendered`）だけrender後URLを使い、`no-gain`・`failed`・`skipped`のときは静的経路の`final_url`を使う（10-fetch-fidelity-design.md §4.2）。robots.txtは静的経路が初期URLと各リダイレクト着地ホストで確認済みのため再確認しない。ただしrender経路では、Chromeが辿るリダイレクトやクライアント側遷移（`meta refresh`、`location.href`）の着地ホストは確認されない（`--render`単体と同じ既知の制約。04 §4の「静的経路と同じ範囲」は実装時にこの表現へ訂正する）。netguardはrender経路の二層防御（04-design.md §3.1）がそのまま働く。
4. render成功時はそのDOMに対してextract（`--raw`なら`strip_non_content`）とconvertをやり直す。render後の可視テキスト長が静的の可視テキスト長より大きければrender結果を採用し`render_status=rendered`、そうでなければ静的結果を採用しstderrに`webgrab: warn=auto-render-no-gain reason=shorter`を出して`render_status=no-gain`（`reason=shorter`）とする（`no-gain`はヘッドレス検知やJSチャレンジでrenderが短いシェルしか得られなかった場合も含む。判断材料としてJSONに`static_chars`と`rendered_chars`を併記する）。採用した本文（スライス前の変換後本文）が0文字なら終了コード6、1〜199文字なら終了コード0 + short-contentマーカーとし、いずれの提案も§4.1の規則（`render_status`基準）で決める。`--max-chars 0`はスライス後が常に空だが判定はスライス前なので終了コード0のまま。`--raw`時は従来どおり終了コード6もshort-contentマーカーも出さず、`render_status`と`[webgrab:render-status ...]`行だけで経路を伝える。
5. renderフェーズの失敗のうち、終了コード7（Chrome未検出・起動失敗・CDPエラー・renderタイムアウト）、renderフェーズでの`--max-bytes`超過、render後のextract/convertの失敗（要素ネスト上限・内部エラー）は、stderrに`webgrab: warn=auto-render-failed reason=<render|max-bytes|extract>`を1行出し、2行目に詳細（§4.4のサニタイズ規則で1行に収める）を出して、静的結果を採用する（`render_status=failed`）。終了コードは静的結果に従い、短文なら0 + short-contentマーカー、空なら6（提案は§4.1の規則により`--raw`）。終了コード8（メインフレームのナビゲーションの内部アドレス到達）だけは、既存契約どおり出力なしで伝播して終了する（静的結果も出さない。理由は§3決定表）。終了コード8はrenderフェーズの他の失敗分類より先に判定する（`--max-bytes`超過が同時に成立しても8を返す）。分類は発生フェーズを引数に取る純関数`fallback_reason(phase, &err) -> Option<&'static str>`（`phase=Render`: 8→None＝伝播、7→`render`、4→`max-bytes`。`phase=Extract`: 任意の`Err`→`extract`）で行い単体テストする（終了コード4は「サイズ超過」と「要素ネスト上限」で多重化されているため、エラー値だけでは区別できない）。
6. 継続コマンド（`[webgrab:truncated ...]`とJSONの`continue_command`）は、`render_status`が`rendered`なら`--auto-render`を`--render`に置換し、それ以外（`static` / `no-gain` / `failed` / `skipped`）なら`--auto-render`を省略し、render系フラグ（`--wait-ms`、`--no-sandbox`、`--chrome-path`）も省略して静的経路を再現する（続き取得のたびに`flag-ignored`や`no-sandbox`警告が出ないようにする）。`--render`と`--auto-render`の同時指定では`--render`を1回だけ出す。`rendered`のときは`--chrome-path`（既に再現済み）と`--no-sandbox`も再現し、置換後の`--render`が同じ環境設定で再現できるようにする。値付きフラグ（`--user-agent`、`--chrome-path`）はURLと同じ`shell_quote`で囲む（現状の素朴なシングルクォート囲みを置換）。render経路の続き取得は再描画結果に依存し、動的ページでは内容が変化しうる（`--render`単体と同じ既知の制約）。

### 4.4 CLI変更（04-design.md §5 フラグ表の差分）

| フラグ | 型 | デフォルト | 挙動 |
|---|---|---|---|
| --auto-render | フラグ | off | 静的取得の可視テキスト（スライス前）が空または200文字未満のとき、同一プロセス内でJSレンダリングに切り替える。`--raw`でも同じ基準で切り替える（`--raw`の終了コード6 / short-content免除は維持）。切替・失敗・skipはstderrの`info=` / `warn=`行、`render_status`、markdown/text/htmlの`[webgrab:render-status ...]`行で通知する。`--render`と同時指定時は`--render`が優先。注意: 外部ページは本文を短くするだけでChrome起動を誘発でき、エスカレーション時は対象オリジンへ描画1ページ分（サブリソース含む）の要求が追加で発生する。自側のコストは残余`--timeout`と残余`--max-bytes`（両経路とも展開後バイト）で上限づける。text/plainはこのエスカレーション判定の対象外 |
| --wait-ms | Option<u64>（未指定=5000。変更前2000） | 5000 | `--render` / `--auto-render`のrender時、`goto`開始からDOM取得までの上限ミリ秒（ナビゲーション時間を含む。変更前は「load後の追加待機」だった）。`--auto-render`時は残余`--timeout`に丸める。ネットワーク静止・DOM安定・可視テキスト200文字以上を満たせば上限前に終了する。`--render`と`--auto-render`のどちらも指定されていない場合は無視しstderrに`warn=flag-ignored flag=--wait-ms`を出す（現状この注記は未実装のため本改訂で実装する。明示指定の判定は`Option`の`Some`で行い、`ArgMatches`は不要） |
| --no-sandbox | フラグ | off | Chromeのsandboxを無効化する。Chromeを実際に起動する直前にstderrへ`warn=no-sandbox`を1行出す（エスカレーションしない実行やskipされた実行では出さない）。あわせて継続コマンドに再現する（stdout側の継続コマンドにこのフラグが載る点はSKILLで注意喚起）。用途はsandboxが起動しないCI環境に限り、通常利用では指定しない（README「セキュリティと信頼モデル」に記載） |
| --chrome-path | パス | なし | 変更なし（継続コマンドには既に再現されている） |

終了コード表は変更しない。stderrの機械可読行の種別に`info=`を加える（`error=` / `warn=` / `info=`。`info=`は失敗を意味せず、失敗する実行では`error=`ブロックが必ず後続する）。追加する行は`info=auto-render`、`warn=auto-render-failed`、`warn=auto-render-no-gain`、`warn=auto-render-skipped`、`warn=extract-grab-failed`、`warn=flag-ignored`、`warn=no-sandbox`、`warn=intercept-build-failed`（Fetch interceptのパラメータ組み立てに失敗し、`request_id`のみで`Fetch.continueRequest`を再試行したとき）。いずれもブロック先頭行は空白を含まないトークンだけで構成し、詳細は2行目以降に出す。詳細行のサニタイズは本文と同じ`strip_terminal_controls`（C0・DEL・C1を除去）を共有し、`\t` `\n` `\r` U+2028 U+2029は空白へ畳み、512バイトを超えない最大の文字境界で切り詰めて`…`を付す（バイト境界での切断はUTF-8の途中でpanicするため禁止）。

JSONエンベロープとfrontmatterに`render_status`（`static` / `rendered` / `failed` / `no-gain` / `skipped`）を追加し、JSONには`static_chars`と`rendered_chars`（可視テキスト長。renderしていなければnull）も加える。`--render`明示時は`rendered`、既定の静的経路は`static`。`--max-chars 0`と終端（`ended`）でも同じ規則で出す。markdown / text / htmlでは`render_status`が`failed` / `no-gain` / `skipped`のときだけ、既存マーカーと同じ書式の`[webgrab:render-status <status> reason=<token>]`（`reason`は`failed`が`render|max-bytes|extract`、`skipped`が`timeout|max-bytes`、`no-gain`が`shorter`）を1行付ける（htmlはコメント。htmlでは本文側の閉じ忘れ`<!--`にマーカーが飲み込まれないよう、マーカー群の直前に`-->`を1つ出す）。位置は`--fence`の閉じ行の外側で、出力順は「本文（フェンス内）→ フェンス閉じ → `[webgrab:truncated ...]`または`[webgrab:end ...]` → `[webgrab:short-content ...]` → `[webgrab:render-status ...]`」に固定する。webgrab自身が生成する行なので偽造無害化の対象外（本文側の同名文字列は従来どおり`[quoted-webgrab:`になる）。`--max-chars 0`でもこの行は出す（自己参照を含まないため既存の抑止対象外。text/htmlでは`[webgrab:meta-only ...]`の直後）。`static` / `rendered`では行を増やさない。`-o`でファイル出力した場合はstdoutが空になるため、この行は出力ファイル側に入り、経路の判別はstderrか出力ファイルで行う。

`static_chars` / `rendered_chars`（可視テキスト長）の値は次の規則で決める。`static_chars`は静的フェーズを実行したら常に値（`--render`明示時のみnull）。text/plainでは抽出HTMLが存在しないため、`static_chars`は本文の文字数になる。`rendered_chars`はrenderを実行してDOMを得たときのみ値（`rendered` / `no-gain`、および`--render`明示時）、`static` / `skipped` / `failed`ではnull。

E2Eハーネスだけが読む環境変数を次に示す（バイナリ本体は環境変数を読まない）。

| 変数 | 型 | 既定 | 影響 |
|---|---|---|---|
| WEBGRAB_E2E | `1`で有効 | 未設定 | 未設定で`CI`が設定されていれば失敗、`CI`も未設定ならskip |
| WEBGRAB_CHROME | パス | 未設定 | 設定時は`--chrome-path`として子プロセスへ渡す（E8を除く） |
| WEBGRAB_E2E_NO_SANDBOX | `1`で有効 | 未設定 | 設定時は`--no-sandbox`を子プロセスへ渡す（CIのtestジョブとcoverageジョブで設定） |

`static_chars`と`rendered_chars`の値の規則は上記のとおり。

### 4.5 文書・サンプルの更新

- `samples/skills/claude/webgrab/SKILL.md`、`samples/skills/codex/AGENTS.md`、`samples/skills/kimi/webgrab-tool.md`: 「使い分け」に`--auto-render`を追加し、終了コード6（`error=empty hint=...`行）と`warn=short-content`の`hint=`は`render_status`に従って出るので、フラグの有無で判断せず`hint=`の値をそのまま使う、と書き換える（現行の「静的なら`--render/--raw`、`--render`時なら`--raw`」という規則を置換。各SKILLの終了コード表の6の行も同じ文言に揃える）。継続コマンドに`--no-sandbox`が含まれうること（必要だった環境の外で実行しない）を注記する。`--auto-render`が外部ページ起因でChrome起動と対象オリジンへの追加要求を誘発しうるため一覧ページや連続取得では既定にしない注意、経路の判別は`--format json` / `frontmatter`の`render_status`か、markdown/text/htmlのstdout末尾側の`[webgrab:render-status ...]`行で行うこと、`no-gain`の意味（render結果が静的以下。ヘッドレス検知で防がれた場合を含む）を明記する
- 配置済みコピー（`~/.claude/skills/webgrab/SKILL.md`、`~/.kimi-code/AGENTS.md`）はリポジトリ外のため完了条件に含めないが、samples更新後に反映した手順を07-verification-report.mdに記録する
- `README.md`: 使い方に`--auto-render`と`--wait-ms`（上限であること。現行READMEに`--wait-ms`の記載は無いため追加）を載せ、「セキュリティと信頼モデル」に`--no-sandbox`の位置づけ（sandboxは敵対HTMLの封じ込め層であり、CI等の限られた環境でのみ無効化する）を追記する
- `docs/04-design.md`: v1.3として次を更新する。§3決定表（レンダリング既定行に`--auto-render`を追記。「暗黙フォールバック不採用」行に「`--auto-render`時の終了コード7に限り、静的結果への復帰を`warn=`と`render_status`で明示して行う」例外を追記）、§4（render.rsの待機、pipelineのエスカレーション、robots確認範囲の訂正）、§5フラグ表（`--auto-render`、`--wait-ms`、`--no-sandbox`）と機械可読行の種別（`info=`追加）と「先頭行」の定義、§5の終了コード6先頭行（`hint=`追加とメッセージの2行目移動）、§5のshort-content提案規則（フラグ基準から`render_status`基準へ置換。skip/failed/no-gainの各値を明記）、§7の「本文抽出0文字 → stderrに--raw/--render提案」（同じく`render_status`基準へ）、§6 json / frontmatter（`render_status`、`static_chars`、`rendered_chars`）とmarkdown等の`[webgrab:render-status ...]`行、継続コマンド規則(1)に`--auto-render`と非エスカレーション時のrender系フラグの例外を追記し、規則(4)として置換・省略と`--no-sandbox`の再現と`shell_quote`を新設、§7（本文抽出失敗の写像、auto-render失敗時の挙動、終了コード8のrender経路メッセージに解決IPとレンジを含めることの明記）、§5フラグ表の`--max-bytes`行（render経路は`dataReceived.data_length`の合計とDOM長を主判定、プロキシのワイヤ計上を第二の保険）、§3.1第一層の「全サブリソース」をページセッション内に限定しOOPIF/Service Workerはプロキシのみと訂正、§3.1第二層に`dataReceived`層を追記、§4のモジュール構成ツリーに`render/wait.rs`を追加、§6 text/htmlの`--max-chars 0`「1行のみ」を`[webgrab:render-status ...]`が続きうる旨に改訂、§8（E2E、カバレッジ文を80と除外解除へ）、§9（CI・カバレッジ80・render.rs除外の解除）。あわせて`src/render.rs`の`max_bytes`のdocコメント（「プロキシで計上」）も訂正する

## 5. モジュール変更

| ファイル | 変更 |
|---|---|
| `src/extract.rs` | `GrabFailed`のみ空本文へ写像し`warn=extract-grab-failed`を出す（§4.1） |
| `src/render.rs` | `drive`の待機を§4.2へ置換。`Network`監視タスクの追加、`main_blocked`のメインフレーム限定と再確認とintercept同期待ち（`goto`失敗経路を含む）、evaluateと`page.content()`のタイムアウト、タスクのabort-on-dropガード、ホスト解決の2秒上限、実効waitの丸め、超過メッセージに利用者指定値を併記、`drive`の結果によらず`main_blocked`を最初に評価する単一経路、`dataReceived`による展開後バイト計上と超過時の中断、`content()`前のDOM長評価、分離ワールドの作成と`context_id`付きevaluate、interceptの個別タスク化（同時16）とホスト判定キャッシュ、遮断件数の`warn=netguard-blocked`通知、deadline基準の実効wait、終了コード8の詳細行に`layer=intercept`と解決IP・レンジを含める（04 §7の既存要求）。`RenderOptions`に`no_sandbox: bool`を追加（`--no-sandbox`から設定。chromiumoxideの`BrowserConfig::builder().no_sandbox()`） |
| `src/render/wait.rs`（新設） | `InFlight`（`RequestId`集合 + tombstone。`Arc<Mutex<_>>`で共有）、`DecodedBudget`、`should_stop`、`effective_cap`、`exceed_msg`、`is_main_navigation(resource_type, frame_id, &main_frame_id)`（`main_frame_id`は手順0で取得済みの非Option）の純関数、定数（POLL_MS=250、STABLE_POLLS=2は「直前との一致回数」、TOMBSTONE_MS=2000、集合上限4096。可視テキスト閾値は既存の短文閾値定数を共有） |
| `src/render/world.rs`（新設） | 分離ワールド（`Page.createIsolatedWorld`）での評価。`IsolatedWorld`（`ensure_ctx` / `eval_numbers` / `measure` / `dom_length` / `dom_html`）と、1回のevaluateに与える上限を`min(deadline残余, cap残余 + 予備2000ms)`へ丸める`eval_limit` |
| `src/render/intercept.rs`（新設） | `Fetch.requestPaused`の個別タスク処理（同時16、`tokio::task::JoinSet`で親タスクのabortに追随）と`Network`4イベントの監視タスク、両者の共有状態`Shared`、`lock` / `sync_wait` / `host_denial` / `netguard_warn_lines` / `netguard_detail`、設置関数`install` |
| `src/renderproxy.rs` | CONNECT/HTTP絶対形式リクエストを解析し、検証済みIPへ双方向転送する。ダウンロード総量を`ProxyState`へ計上し`--max-bytes`超過を検出。ホスト解決は`renderproxy/hostcache.rs`へ委譲 |
| `src/renderproxy/hostcache.rs`（新設） | 宛先ホスト解決に2秒上限を設け、超過はfail-closed。判定キャッシュをrender.rsと共有し`HostCache`として公開 |
| `src/fetch.rs` | `Fetched`に展開後の消費バイト数（最終応答の本文長。リダイレクト中間応答とrobots.txtは含めない）を追加し、残余`--max-bytes`の計算に使う |
| `src/pipeline.rs` | 静的フェーズ / エスカレーション判定 / renderフェーズに分割（§4.3）。Chrome非依存の純関数として`escalation_reason(visible_chars) -> Option<&'static str>`、`remaining_budget(timeout, elapsed, max_bytes, consumed) -> Result<(Duration, u64), SkipReason>`（`enum SkipReason { Timeout, MaxBytes }`）、`choose_result(static_chars, rendered_chars) -> RenderStatus`、`fallback_reason(Phase, &WebgrabError) -> Option<&'static str>`、`hint_for(render_status) -> (&'static str, &'static str)`（stderr用トークン`--render/--raw`と本文用散文`--render or --raw`）を切り出して単体テストする。`RenderStatus`は`src/output.rs`に定義しpipeline・cliから参照する。`--wait-ms`の`flag-ignored`注記 |
| `src/cli.rs` | `--auto-render`と`--no-sandbox`の追加、`--wait-ms`を`Option<u64>`にして未指定を`None`で表す（`DEFAULT_WAIT_MS=5000`は使用時に適用。`flag-ignored`注記と継続コマンド再現は`Some`で判定）、`extra_flags(cli, render_status)`で置換・省略規則（§4.3 6）、値付きフラグの`shell_quote`（`budget::shell_quote`を`pub(crate)`に上げて共有） |
| `src/output.rs` | `Meta`に`render_status`・`static_chars`・`rendered_chars`を追加（既存テストの`Meta`リテラルに`..Default::default()`を足してから追加する）、JSONとfrontmatterへ出力、markdown/text/htmlの`[webgrab:render-status ...]`行 |
| `src/error.rs` | stderrブロック先頭行に付加する`key=value`トークンを保持する手段（`with_token`）を追加。詳細行のサニタイズ（§4.4の規則、`strip_terminal_controls`を共有） |
| `src/convert.rs` | `visible_text_len(html) -> usize`（タグ・script・style・noscript除去、空白畳み込み、失敗しない）を追加 |
| `src/budget.rs` | 変更なし（継続コマンドは`extra_flags`の結果を使う） |
| `tests/common/mod.rs`（新設、`#![allow(dead_code)]`） | 任意回数のリクエストに応答し、パスごとに本文（`Vec<u8>`）・Content-Type・任意の追加ヘッダ（`Content-Encoding`等）・遅延を設定できる最小HTTPサーバ（Chromeはfavicon等も要求するため、回数固定の既存サーバでは足りない。`Content-Length`は送信バイト長）。E2Eを直列化する`Mutex` |
| `tests/fixtures/big_gzip.html.gz`（新設） | 展開後2MiB（`x`の繰り返し + `SENTINEL_GZIP`）を`gzip -9`した約2.1KiBのバイナリ。生成コマンドを`tests/fixtures/README.md`に記す |
| `tests/integration.rs` | F1の回帰テスト、`hint=`トークンの検証、skip契約の検証（短文150文字の静的fixtureに`--auto-render --timeout 3`を与え、残余が閾値未満で必ずskipすることを使って、stderrの`warn=auto-render-skipped reason=timeout`とstdoutの`[webgrab:short-content` → `[webgrab:render-status skipped reason=timeout]`の順序を検証。空本文でのskipは終了コード6と`hint=--render/--raw`として別ケース）を追加（Chrome不要） |
| `tests/render_e2e.rs`（新設） | §6のE2E |
| `.github/workflows/ci.yml`（新設） | §7 |

## 6. テスト戦略

### 単体（Chrome不要）

- `extract`: 空シェルHTML（`<div id="app"></div>`のみ）で`Ok`かつ本文空。記事HTMLで従来どおり本文あり
- `wait`: `InFlight`はリダイレクト再送（同一ID、`is_redirect=true`）で件数が増えない、未知IDの`on_done`で負にならない、挿入→削除で空になる、`on_done`→`on_request`の順でも空のまま（tombstone）。`should_stop`は (idle, stable=2, text=200, elapsed<cap)→true、idle=false→false、stable=1→false、text=199→false、elapsed>=capなら他条件によらずtrue
- `cli`: `--auto-render`と`--no-sandbox`の解析。`extra_flags`は`render_status=rendered`で`--auto-render`が`--render`に置換され、`static`/`failed`/`no-gain`/`skipped`では`--auto-render`が出ず、`--render --auto-render`では`--render`が1回だけ出る。`--wait-ms 2000`は再現され、既定値と同じ`--wait-ms 5000`も明示指定なら再現される（未指定のときだけ出ない）。`--chrome-path`と`--no-sandbox`が再現される
- `pipeline`: `escalation_reason`（0→`empty`、199→`short`、200→None）。`remaining_budget`（経過が`--timeout`−5秒以上なら`Err(Timeout)`、残余バイトが256KiB未満なら`Err(MaxBytes)`、それ以外は`Ok(残余)`）。`choose_result`（rendered>static→`rendered`、それ以外→`no-gain`）。`fallback_reason`（`Render`フェーズ: 終了コード8→None、7→`render`、4→`max-bytes`。`Extract`フェーズ: 終了コード4→`extract`、1→`extract`）。`hint_for`（`static`/`skipped`→(`--render/--raw`, `--render or --raw`)、他→(`--raw`, `--raw`)）
- `convert`: `visible_text_len`（`href`が30文字超のリンク10個からなるナビ付き空シェルでアンカーテキスト分（各2文字×10=20）だけが数えられURLは含まれない、記事→本文文字数、script/style内は数えない）
- `wait`（追加）: `is_main_navigation`（Document + メインフレームID→true、Document + 別フレームID→false、Image→false）。`DecodedBudget::on_data(len) -> bool`（累積が上限を超えた最初の呼び出しでtrue、以後もtrue）。`host_cache`（同一`host:port`の2回目は解決を呼ばない）
- `extract`（追加）: F2のプレースホルダmarkup `<div id="app">読み込み中...</div>`が`Ok`かつ本文8文字（E7の前提を固定）
- `output`: JSONとfrontmatterに`render_status`が出る。`static_chars`/`rendered_chars`のnull規則。markdownで`failed`のとき`[webgrab:render-status failed reason=render]`行が出て、`rendered`では出ない。本文中の同名文字列は既存の偽造無害化で`[quoted-webgrab:`になる
- `error`: 先頭行トークンの付与と、詳細のC1・改行が除去または畳まれ512バイトで切られること

### 統合（Chrome不要、`tests/integration.rs`）

- 空シェルHTMLを静的取得 → 終了コード6、stderrに`webgrab: error=empty hint=--render/--raw`で始まる行がある（F1回帰）
- `--wait-ms 100`を`--render`なしで指定 → stderrに`warn=flag-ignored flag=--wait-ms`

### E2E（実Chrome、`tests/render_e2e.rs`）

有効化と環境変数は§4.4の表のとおり（`WEBGRAB_E2E` / `WEBGRAB_CHROME` / `WEBGRAB_E2E_NO_SANDBOX`）。テストは`Mutex`で直列化し、CIでは`--test-threads=1`でも実行する。fixtureはすべてローカルサーバが配信し、E9以外は`--allow-private --no-robots`で取得する（E9は`--no-robots`のみ）。各fixtureは固有の番兵文字列を持ち、判定は番兵の`contains`で行う。

| fixture | 内容 | 番兵 |
|---|---|---|
| csr_fast | 空シェル、500ms後にJSで記事（400文字超）を挿入 | `SENTINEL_FAST` |
| csr_slow | `<div id="app">読み込み中...</div>`、2500ms後にJSで記事（400文字超）を挿入（F2の再現markupをそのまま使う。プレースホルダが本文8文字として抽出されることは§1のprobeと`extract`単体テストで固定。変更前の固定待機2000msでは取れず、既定上限5000msには2.5秒の余裕がある） | `SENTINEL_SLOW` |
| csr_xhr | `fetch('/api/data')`の応答（サーバ側1000ms遅延）で記事（400文字超）を描画 | `SENTINEL_XHR` |
| static_article | 静的な記事（400文字超） | `SENTINEL_STATIC` |
| short_static | 静的150文字の本文、JSは20文字のシェルに置換する（JSチャレンジ模擬） | `SENTINEL_SHORT` |
| big_gzip | `tests/fixtures/big_gzip.html.gz`（展開後2MiB、約2.1KiB）を`Content-Encoding: gzip`、`Content-Length`=圧縮後長で返す | `SENTINEL_GZIP` |
| dom_bomb | 1KiBの文書で、JSが`document.body.innerHTML`に3MiB分のテキストを生成する（ネットワークを経ないDOM膨張） | `SENTINEL_DOM` |

| ケース | fixture | コマンド | 期待 |
|---|---|---|---|
| E1 | csr_fast | `--render` | 終了コード0、stdoutに`SENTINEL_FAST` |
| E2 | csr_slow | `--render --wait-ms 8000` | 終了コード0、stdoutに`SENTINEL_SLOW`（F2回帰。挿入2500ms + 安定判定最短750msに対し既定5000msでは余裕が約1.7秒と細いため上限を明示。既定値の検証はE1が担う） |
| E3 | csr_xhr | `--render` | 終了コード0、stdoutに`SENTINEL_XHR` |
| E4 | csr_fast | `--auto-render` | 終了コード0、stdoutに`SENTINEL_FAST`、stderrに`info=auto-render reason=empty`、stdoutに`short-content`なし |
| E5 | static_article | `--auto-render` | 終了コード0、stdoutに`SENTINEL_STATIC`、stderrに`auto-render`を含む行なし |
| E6 | csr_fast | `--auto-render --format json --max-chars 50` | `render_status`が`rendered`、`continue_command`に`--render`を含み`--auto-render`を含まない |
| E7 | csr_slow | `--render --wait-ms 1500` | stdoutに`SENTINEL_SLOW`なし、かつ「終了コード0 + `[webgrab:short-content`」または「終了コード6 + `error=empty`」のいずれか（上限が効く。負荷でナビゲーションが遅れた場合の後者も許容） |
| E8 | csr_fast | `--auto-render --chrome-path /nonexistent`（`WEBGRAB_CHROME`を無視） | 終了コード6、stderrに`error=empty hint=--raw`で始まる行と`warn=auto-render-failed reason=render`で始まる行がある |
| E9 | csr_fast | `--render --no-robots`（`--allow-private`なし。robots事前確認を飛ばしてrender内の遮断を通す） | 終了コード8、stderrに`error=netguard` |
| E10a | short_static | `--auto-render` | 終了コード0、stdoutに`SENTINEL_SHORT`と`[webgrab:render-status no-gain reason=shorter]`、stderrに`warn=auto-render-no-gain reason=shorter` |
| E10b | short_static | `--auto-render --format json` | `render_status`が`no-gain`、`rendered_chars` < `static_chars` |
| E11 | csr_fast | `--auto-render --format json --max-chars 0` | 終了コード0、`render_status`が`rendered`、`markdown`が空文字列（判定がスライス前全文で行われる） |
| E12 | csr_fast | `--auto-render --raw --format json` | `render_status`が`rendered`、`markdown`に`SENTINEL_FAST`（`--raw`でも可視テキスト基準で発火する） |
| E13 | big_gzip（gzip圧縮で約2.1KiB、展開後2MiBの本文を`Content-Encoding: gzip`で返す） | `--render --max-bytes 1048576` | 終了コード4、stderrに`error=http`（展開後バイトで上限が効く。ワイヤ2.1KiBでは超過しない） |
| E14 | big_gzip | `--auto-render --max-bytes 1048576`（静的経路は`read_capped`で先に超過し終了コード4）| 終了コード4、stderrに`error=http`（静的フェーズのエラーは伝播、§4.3 1） |
| E15 | dom_bomb | `--render --max-bytes 1048576` | 終了コード4、stderrに`error=http`（`content()`前のDOM長評価が効く） |
| E16 | static_article | `--render --no-sandbox` | 終了コード0、stderrに`webgrab: warn=no-sandbox`の行がちょうど1本（Chromeを起動する実行でだけ出る、§4.4） |

## 7. CI設計（`.github/workflows/ci.yml`）

逸脱の記録: ユーザーのCI慣例は中央`okamyuji/reusable-workflows`の薄い呼び出しだが、中央にrust用workflowが無く、Chrome付きE2Eがこのリポジトリ固有のため、check / test / coverageはリポジトリ内に直接書く。security-scanは慣例どおり中央を呼ぶ。既定ブランチは`master`。runnerは`ubuntu-24.04`に固定し、26.04への移行は同梱Chromeとsandbox挙動を再確認してから行う。

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
      - uses: actions/checkout@<40桁SHA> # v7（実装時に解決）
        with:
          persist-credentials: false
      - uses: dtolnay/rust-toolchain@<40桁SHA> # stable
        with:
          components: rustfmt, clippy
      - uses: Swatinem/rust-cache@<40桁SHA> # v2
      - run: '! grep -nE "uses: .*@(v[0-9]|stable|cargo-llvm-cov)" .github/workflows/ci.yml | grep -v reusable-workflows'
      - run: |
          test "$(grep -c 'persist-credentials: false' .github/workflows/ci.yml)" -eq "$(grep -c 'uses: actions/checkout@' .github/workflows/ci.yml)"
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
      - uses: actions/checkout@<40桁SHA> # v7
        with:
          persist-credentials: false
      - uses: dtolnay/rust-toolchain@<40桁SHA> # stable
      - uses: Swatinem/rust-cache@<40桁SHA> # v2
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
      - uses: actions/checkout@<40桁SHA> # v7
        with:
          persist-credentials: false
      - uses: dtolnay/rust-toolchain@<40桁SHA> # stable
        with:
          components: llvm-tools-preview
      - uses: taiki-e/install-action@<40桁SHA> # cargo-llvm-cov
      - uses: Swatinem/rust-cache@<40桁SHA> # v2
      - run: google-chrome --version
      - run: cargo llvm-cov --lib --bins --test integration --test render_e2e --fail-under-lines 80 -- --test-threads=1
  security:
    permissions:
      contents: read
      pull-requests: write
    uses: okamyuji/reusable-workflows/.github/workflows/security-scan.yml@v1
```

`<40桁SHA>`は実装時に各actionの該当タグのコミットSHAへ置換する（YAML内のコメントに元タグを残す。sandbox無効のChromeを走らせるためサプライチェーン面を狭める）。`security-scan.yml@v1`は中央リポジトリの運用方針（タグ張り替えで追随）に従い例外とし、checkジョブのgrepからも除外する。testとcoverageでE2Eを二重実行するのは許容する（coverageは計装ビルドで、testは素のビルドの挙動を見る）。sandboxはCIでは最初から無効化する（使い捨てVMで対象がローカルfixtureのみ。Ubuntu 24.04のuser namespace制限で起動しない事例があるため）。初回CI実行でsandbox有効のまま動くことが観測できれば`WEBGRAB_E2E_NO_SANDBOX`を外し、結果を07-verification-report.mdに記録する。coverageが80を割った場合は`--ignore-filename-regex 'render\.rs'`を戻し、差分をバックログに記録する。

## 8. ブラウザ実動作検証（PR作成前）

1. `cargo build --release`のバイナリで、ローカルfixture（csr_fast / csr_slow / csr_xhr）と実在のJS描画ページ3件（`https://react.dev/learn`、`https://demo.playwright.dev/todomvc/`、`https://qiita.com/`）を`--render`および`--auto-render`で取得し、出力をファイルに保存する
2. 同じURLをChrome（claude-in-chrome）で開き、表示テキストから見出し1つと本文の先頭段落（30文字以上）を抜き出して、取得結果に完全一致で含まれることを確認する
3. 結果（URL、比較した文字列、一致/不一致、終了コード、`render_status`）を`docs/07-verification-report.md`に「JS描画改善の検証（2026-08-27）」として表で記録する

## 9. 完了条件（機械検証可能）

1. `cargo test`が終了コード0（E2Eは`WEBGRAB_E2E=1`付きでローカルでも0）
2. `cargo clippy --all-targets -- -D warnings`が終了コード0
3. `python3 tools/doclint.py docs/`が`Critical 0 / High 0`
4. §6 E1〜E9、E10a、E10b、E11〜E15がCIの`test`ジョブで実行され（skipでなく）すべて合格
5. `cargo llvm-cov ... --fail-under-lines 80`が終了コード0。除外なし（§7のコマンドそのまま）で達成するか、`--ignore-filename-regex 'render\.rs'`を戻して達成し差分をバックログに記録するかの二択で、どちらを採ったかを07-verification-report.mdに記録する
6. §4.2・§4.3で追加する各機構に対応する単体テストまたはE2Eが存在する（`InFlight`・`should_stop`・`is_main_navigation`・`remaining_budget`・`choose_result`・`fallback_reason`・`hint_for`・`visible_text_len`は単体、skip契約は統合テスト、他はE1〜E12）
7. §8の突合結果（Chromeコールドスタートの実測Lを含み、skip閾値5秒が「L + 2000ms + 1000ms」以上であることを確認。早期終了したか上限到達だったかも記録）が07-verification-report.mdに記録されている
9. `.github/workflows/ci.yml`の全`uses:`が40桁のコミットSHAで、`actions/checkout`に`persist-credentials: false`がある（checkジョブの`grep`で機械検証）
8. PRのCIがすべて緑（CodeRabbitの指摘への対応は人手の完了条件として別途扱う）

## 10. やらないこと（再掲）

既定でのChrome起動、`--render`明示時の暗黙静的フォールバック、中央reusable-workflowsの変更、ブラウザ指紋偽装、Cookie/ログイン、スクロールやクリック等のページ操作、OOPIF/Service Workerの子セッションへの`Fetch`/`Network`付与（次版候補）。

## 変更履歴

| 版 | 日付 | 変更 |
|---|---|---|
| 1.0 | 2026-08-27 | 初版 |
| 1.6 seal | 2026-08-27 | 実装前probe(13)〜(15)の補遺。分離ワールドは`goto`後に作成、DOM HTMLの取得を`page.content()`から分離ワールド評価へ変更（ページ側のgetter上書きによる偽装を防ぐ）、`tokio::sync`は変更不要 |
| 1.5 seal | 2026-08-27 | Round 4（最終）反映。終了コード8を`drive`の結果によらず先に評価する単一経路、`content()`前のDOM長評価（JSによるDOM膨張の上限）、分離ワールドでのevaluate、メインフレームIDの取得時期と失敗時の終了コード7、interceptの個別タスク化とホスト判定キャッシュ、プロキシ遮断の`warn=netguard-blocked`通知、deadline基準の実効waitとskip閾値5秒、`fallback_reason(phase, err)`、`--wait-ms`の`Option`化、`shell_quote`の共有、文字境界での切り詰め、htmlの`-->`ガード、OOPIF/Service Workerの限界を定量で明記（probe(10)〜(12)）、`tests/common`のバイト本文と任意ヘッダ、`tests/fixtures/big_gzip.html.gz`、dom_bomb fixtureとE15、E2の`--wait-ms 8000`、`ubuntu-24.04`固定、YAML内のSHAピン留めと`persist-credentials: false`と機械検証、§4.5に04 §3.1/§4/§5 `--max-bytes`/§6/§8の更新を追加、SKILL終了コード表と`--no-sandbox`注記、README文言 |
| 1.4 | 2026-08-27 | Round 3所見を一次情報（コード・crateソース・probe）で検証し全件正当と確認（§1 probe(7)〜(9)）。render経路の`--max-bytes`を`Network.dataReceived.data_length`（展開後）の合計で計上する主判定に変更し、プロキシのワイヤ計上を第二の保険に位置づけ（gzip爆弾の根本対処）。E13/E14とbig_gzip fixture、`DecodedBudget`単体を追加 |
| 1.3 | 2026-08-27 | レビューRound 3反映（ラウンド上限のため独立再レビューなし）。終了コード8の優先判定、`main_blocked`のメインフレーム限定（`is_main_navigation`）、`goto`失敗経路の8/7判定、`wait_for_navigation`上限1000ms、`page.content()`タイムアウトと予備2000ms、`SkipReason`型、`visible_text_len`（URLを含まない）と非対称の明記、`no-gain reason=shorter`、マーカー出力順とフェンス外配置、`--max-chars 0`でのrender-status行と終了コード0、`static_chars`/`rendered_chars`のnull規則、非エスカレーション時のrender系フラグ省略、`shell_quote`、`value_source`、`hint_for`のタプル、`RenderStatus`の定義位置、`fallback_reason`、tombstoneのTTLと上限、限界(f)(g)、終了コード8詳細行に層とIP、skip契約の統合テスト、E7の許容緩和、E11の終了コード、fixture本文長、§9の範囲と番号、04更新項目の追加、actionのSHAピン留め、skip閾値の実測前提 |
| 1.2 | 2026-08-27 | レビューRound 2反映。intercept側削除を`network_id`キーに、tombstone、要素数+innerText長へ変更、`evaluate_expression`、最短750ms、実効waitの残余丸め、intercept同期待ち、renderproxyの解決上限、判定量を可視テキストに統一、静的エラー時は伝播、skip閾値3秒/256KiB、renderフェーズのmax-bytes超過・extract失敗も静的へ復帰（伝播は終了コード8のみ）、`--raw`の免除維持、`hint=`を`render_status`基準に統一、「先頭行」の定義、`info=`文の訂正、詳細行サニタイズ規則、`static_chars`/`rendered_chars`、`[webgrab:render-status ...]`行、`--no-sandbox`フラグ化とE2E環境変数表、`--chrome-path`既再現の訂正、E9に`--no-robots`、E10分割、E12追加、csr_slow 2500ms、coverageにE2Eを含めrender.rs除外を解除、workflowのpermissions/timeout、probe実測の記録 |
| 1.1 | 2026-08-27 | レビューRound 1反映。in-flightを`RequestId`集合に変更、evaluateの安全化と失敗時扱い、`main_blocked`再確認、タスクのabort-on-drop、ホスト解決上限、`--auto-render`の判定をスライス前全文に固定、`--raw`対応、フォールバックを終了コード7に限定、render結果と静的結果の比較採用、残余`--timeout`/`--max-bytes`の引き継ぎとskip、final_urlの統一、robots確認範囲の訂正、stderr契約準拠（`info=`種別追加、`hint=`トークン、詳細2行目）、`render_status`列挙、継続コマンド規則の修正と`--chrome-path`再現、`--wait-ms`定数化と`flag-ignored`実装、E2Eの番兵化・直列化・`CI`下でのskip禁止・E9〜E11追加、CIを実YAMLで記述しカバレッジ80に変更、限界(c)(d)の明記 |
