# webgrab 設計書

- バージョン: 1.4（[08-js-render-design.md](08-js-render-design.md)（`--auto-render`によるJSレンダリング自動エスカレーション）と[10-fetch-fidelity-design.md](10-fetch-fidelity-design.md)（取得忠実度の改善）の実装を反映。変更履歴は末尾）
- 日付: 2026-08-27
- 根拠文書: [01-measurement-report.md](01-measurement-report.md)（問題の実測）, [03-research-report.md](03-research-report.md)（技術選定の根拠）, [08-js-render-design.md](08-js-render-design.md)（`--auto-render`の差分設計）, [10-fetch-fidelity-design.md](10-fetch-fidelity-design.md)（text/plain素通し・render後URL・切り詰め改行境界・403提案の差分設計）

## 1. 目的と背景

実測により、Claude系エージェントの標準Web取得は次の制約を持つことが確認された。WebSearchはリンクと約1,900文字の生成要約のみを返し、WebFetchは取得内容を100,000文字で切断したうえ小型モデルの要約応答しかメインモデルに渡さない。webgrabは、Webページの本文を切り捨てずに・制御可能な量で・LLMが読みやすいMarkdownとして標準出力に返す単一バイナリCLIであり、Claude Code / Codex CLI / Kimi CLI等のターミナルエージェントがBash経由で呼び出すことを想定する。

## 2. スコープ

- 対象: 単一URLの取得、文字コード判定、本文抽出、Markdown変換、文字量制御、JSレンダリング（オプション）
- 対象外（ユーザー決定によるスコープ凍結）: 検索エンジン統合、リンク追跡クローリング、キャッシュ、crates.io公開

## 3. 決定表（MECE、全行確定済み）

| 決定 | 選択肢 | 採用 | 理由（詳細は03-research-report.md） |
|---|---|---|---|
| HTTP取得 | reqwest / ureq | reqwest | JSレンダリングでtokioが必要なため非同期が自然。デファクト。本文取得は`Accept: text/html,application/xhtml+xml,text/plain;q=0.9,*/*;q=0.8`をHTML優先で固定送信する。robots.txt取得は既定の`*/*`のまま |
| JSレンダリング | chromiumoxide / headless_chrome / サブプロセス | chromiumoxide（tokio専用・デフォルトfeatureで動作。§8のsmoke testで既定featureのままtokioで動くことを検証済み。特別なfeature指定は不要） | CDP制御の細かさとメンテ活発さ。--dump-domは完了待ち制御不能。調査報告書B枝の「Chrome未検出時は静的取得へ暗黙フォールバック」案は不採用（取得内容が静かに変わるsilent degradationを避け、終了コード7でエージェントに判断を返す）。例外: `--auto-render`時の終了コード7に限り、静的結果への復帰を`warn=`と`render_status`で明示して行う（08-js-render-design.md §4.3 5） |
| 本文抽出 | dom_smoothie / readability / llm_readability | dom_smoothie | readability.js忠実移植で2026-06更新。第三者ベンチで旧readability系は本文53バイトのみ返す失敗例あり |
| HTML→Markdown | htmd / fast_html2md / html2md | htmd | 品質・DL数（117万/直近）・Apache-2.0。html2mdはGPLかつ出力肥大例あり |
| トークン概算 | tiktoken-rs / 文字数近似 | tiktoken-rs（o200k_base）。ただし量の制御は文字ベースのみで、トークンによる切り詰めは非対応と契約に明記。計数は出力直前の遅延実行とし、--no-tokensで省略可（起動レイテンシ対策） | 日本語で文字数/4近似は過小見積もりし予算超過を招く。安全側のBPE実測 |
| 文字コード | encoding_rs単体 / +chardetng | encoding_rs + chardetng | reqwestのtext()はmeta charsetを見ないため、ヘッダ→metaスニッフ→推定の3段判定が定石（text/plainはHTML metaの走査を省き、ヘッダ→推定の2段で判定する） |
| robots.txt解析 | 外部crate / 自前最小実装 | 自前最小実装（User-agent / Disallow / Allow。前置一致に加えRFC 9309の`*`と`$`をサポート。解釈できないパターンに一致候補がある場合は安全側=disallow扱い+stderr注記） | 必要仕様が小さく枯れているため。ladder: これ以上の仕様が必要になったらcrate導入 |
| 内部アドレスの取得 | 無制限 / デフォルト拒否+明示解除 | デフォルト拒否+--allow-privateで解除。詳細な拒否レンジと判定方式は§3.1 | SSRF防止（レビュー合意所見。エージェントは注入されたURLをそのまま渡しうる） |
| CLI引数解析 | clap / 自前 | clap v4（derive） | デファクト。終了コード2が引数エラーの慣例と一致 |
| 続き取得モデル | offsetページング / トークン予算拒否 | MCP fetchサーバ互換の--start-index/--max-chars + 自己記述フッタ | エージェントが再呼び出しで続きを取る実績ある操作モデル |
| デフォルト出力形式 | Markdown / テキスト / JSON | Jina Reader互換ヘッダ付きMarkdown | ユーザー決定+デファクト形式 |
| レンダリング既定 | 常時Chrome / 常時静的 / 静的既定+明示フラグ | 静的既定 + --renderフラグ。加えてopt-inの`--auto-render`（既定off）で、静的取得の可視テキストが空または200文字未満のとき同一プロセス内でJSレンダリングへ自動エスカレーションする（詳細は08-js-render-design.md §4.3） | 速度と依存の軽さ。本文が短い場合の再実行提案は7章。`--auto-render`は既定挙動を変えずエージェントの再呼び出しを不要にする |

「文字」の定義: 本書のすべての文字数（--start-index、--max-chars、フッタの範囲表記、chars値）はUnicodeスカラー値（Rustの`char`）で数える。スライスは`char_indices`ベースで行い、バイト境界パニックを起こさない。範囲表記は半開区間`[start, 実際の終端)`とする。切り詰めが発生する場合、終端は範囲後半の最後の改行の直後まで戻すため、出力は`--max-chars`より短くなることがある（詳細は10-fetch-fidelity-design.md §4.4）。

## 3.1 SSRF防止（netguard）の詳細仕様

Round 2レビューで、当初の「名前解決後IPを判定」だけではDNSリバインディング（TOCTOU）とrender経路のサブリソース取得を防げないことが判明したため、次を契約とする。

### 拒否レンジ（--allow-private未指定時）

判定前にアドレスを正規化する。IPv4-mapped IPv6（`::ffff:0:0/96`）とIPv4-compatible IPv6は対応するIPv4へ変換してから両体系のレンジ照合を行う。以下を拒否する。

- IPv4: ループバック`127.0.0.0/8`、リンクローカル`169.254.0.0/16`（AWS/GCPメタデータの`169.254.169.254`を含む）、RFC1918（`10/8`・`172.16/12`・`192.168/16`）、CGN`100.64.0.0/10`、`0.0.0.0/8`
- IPv6: ループバック`::1`、未指定`::`、リンクローカル`fe80::/10`、ULA`fc00::/7`

### 判定と接続の一致（DNSリバインディング対策）

netguardはホスト名を自前で解決し、得た全IPを判定する。合格したIPをそのまま接続先に固定（IPピン留め）してからHTTPクライアントに渡す。具体的にはreqwestの`ClientBuilder::resolve(host, addr)`で検証済みIPを固定し、クライアント側の再解決を避ける。これにより「判定したIP」と「接続するIP」の不一致を排除する。

### リダイレクトの手動追従

reqwestの自動リダイレクトは無効化（`redirect::Policy::none()`）し、CLIが手動でホップを処理する。各ホップで「解決 → netguard判定 → IPピン留め → robots確認（クロスホスト時）」を本文取得前に行う。最大10ホップ、超過は終了コード3。これにより初回URLで合格後に内部アドレスへ302される迂回を塞ぐ。

### render経路のサブリソース遮断

--render時はChromeが独自にDNS解決・サブリソース取得（img/script/iframe/fetch）を行うため、CLI側の事前チェックが効かない。二層で防御する。

第一層はCDPの`Fetch`ドメインによるrequest interceptionで、メインナビゲーションとページセッション内の全サブリソースのリクエストごとに宛先ホストをnetguardで判定する（クロスオリジンiframeとService Workerの要求は別セッションのため第一層には現れない。§3.1「render経路のサブリソース遮断」参照）。内部アドレス宛は`Fetch.failRequest`で遮断し、名前解決に失敗した場合も遮断側に倒す（fail-closed）。ホスト名解決には2秒の上限を設け、超過も遮断側に倒す。`file:`スキームの要求は宛先ホストの判定によらず常に遮断する（`--allow-private`は内部アドレスの許可であり、ローカルファイルの読み出しは許可しない）。`data:`・`blob:`・`about:`はChromeに任せる。判定結果は実行単位のキャッシュへ保存し、renderproxy（第二層）と共有して同一ホストの二重解決を避ける。メインナビゲーション（`resource_type == Document`かつメインフレームIDに一致する要求）が内部アドレスへ到達した場合は終了コード8で中断する。サブフレーム（iframe）やサブリソースの内部アドレス要求は`Fetch.failRequest`で遮断するだけで終了コード8は立てない。遮断件数は`webgrab: warn=netguard-blocked layer=intercept count=N`（第二層は`layer=proxy`）でstderrに通知する。

第二層はChromeの前段に置く検証・IPピン留めローカルプロキシ（src/renderproxy.rs）である。`--proxy-server=127.0.0.1:<port>`と`--proxy-bypass-list=<-loopback>`でChromeの全接続（loopback宛とクロスオリジンiframe・Service Workerの別セッション要求を含む）をプロキシ経由に強制する。プロキシはCONNECT/絶対形式HTTPの宛先ホストを自前で解決・netguard判定（fail-closed、ホスト名解決2秒上限）し、合格した検証済みIPへ接続を固定してから中継する。CONNECTはTLS生バイトをそのままピン留め先へ双方向転送し、HTTPは`Connection: close`を強制して1接続=1リクエストとする。Chrome自身にDNS解決・接続をさせないため、第一層の判定後にChromeが再解決して内部IPへ切り替えるDNSリバインディング（TOCTOU）を原理的に閉じ、静的経路のIPピン留め（§3.1）と同等の保証をrender経路へ与える。第一層がページセッション内の要求を判定し、第二層が別セッション（OOPIF/Service Worker）を含む全接続とダウンロード総量（`--max-bytes`のワイヤ計上、第二の保険）を有界にする二重防御の役割分担となる。

`--timeout`超過時は終了コード7でレンダリングを強制中断する。終了コード8の詳細行は静的経路・render経路とも`host=<host> resolved=<ip> range=<range>`の書式にそろえる。名前解決に失敗した場合は`resolved=unresolved`、2秒の解決上限を超えた場合は`resolved=timeout`とし、いずれも`range=`を省く。

## 4. アーキテクチャ

パイプライン構成で、各ステージは独立にテスト可能なモジュールとする。

```text
URL → [netguard] → [robots] → [fetch | render] → 生バイト+ヘッダ → [decode] → UTF-8 HTML
    → [extract] → 本文HTML+メタデータ → [convert] → Markdown → [budget] → [output] → stdout
```

- netguard（src/netguard.rs）はURL検証（http/httpsのみ）とIPピン留めを含む内部アドレス判定を行う（詳細§3.1）。静的経路・render経路・robots取得・リダイレクト各ホップのすべてがこのモジュールを通る
- robots（src/robots.rs）はfetch前に対象ホストのrobots.txtを確認する（--no-robotsでスキップ）。--render経路でも同様にトップURLのrobots.txtを確認してから起動する（--no-robotsでスキップ）。ただしrender経路では、Chromeが辿るリダイレクトやクライアント側遷移（`meta refresh`、`location.href`）の着地ホストは確認されない（静的経路の全リダイレクトホップ確認とは範囲が異なる。--auto-renderのエスカレーション時は静的経路の`final_url`とその各リダイレクトホップで確認済みのため再確認しない）。robots.txt取得は--user-agentと同じUA・5秒タイムアウト・512KiBサイズ上限で行い、取得先もnetguardを通す。robots.txt応答のリダイレクト追従は最大1回かつ追従先をnetguardで再検証する。512KiB超過や取得失敗は「許可」とみなしstderrに注記して継続する。クロスホストリダイレクトが発生した場合は最終ホストのrobots.txtも確認する。robots.txt内の`User-agent:`行との照合は製品トークン`webgrab`（大文字小文字無視）で行い、--user-agent上書き時も変わらない。一致グループがなければ`*`グループを適用する。ワイルドカード`*`と行末`$`をサポートし、解釈できないパターンに一致候補がある場合は安全側（disallow扱い+stderr注記）に倒す
- fetch（src/fetch.rs）は静的取得（reqwest）。--render指定時はrender（src/render.rs、chromiumoxide）がDOM安定後のHTMLを返す。Chrome未検出・起動失敗時は終了コード7で失敗し、stderrに静的取得への切替コマンドを提示する（暗黙フォールバックはしない）。`--auto-render`（既定off）は静的取得の可視テキストが空または200文字未満のとき同一プロセス内でrenderへ自動エスカレーションする。pipeline（src/pipeline.rs）は「静的フェーズ → エスカレーション判定 → renderフェーズ → 出力」の順で構成し、エスカレーション時の失敗のうち終了コード7・`--max-bytes`超過・render後の抽出失敗は静的結果へ復帰し（`render_status=failed`）、終了コード8（内部アドレス到達）だけは既存契約どおり出力なしで伝播する（詳細は08-js-render-design.md §4.3）
- render.rsは一時user-data-dirを生成し、正常・異常終了ともDropガードでChromeプロセスをkillして一時ディレクトリを削除する。`goto`以降の待機は「ネットワーク静止（`RequestId`集合が空）かつDOM安定（要素数と`innerText`長が直前と2回連続一致）かつ可視テキスト200文字以上」を250msごとに判定し、満たせば早期終了、満たさなければ`--wait-ms`（既定5000ms、上限）まで待つ。判定と計数はsrc/render/wait.rsの純関数（`InFlight`、`should_stop`、`is_main_navigation`等）に切り出しChromeなしで単体テストする
- decode（src/decode.rs）はcharset判定を「HTTPヘッダ → HTML先頭1024バイトのmeta → chardetng推定」の順で行う。--render経路はCDPが常にUTF-8のDOM文字列を返すためdecodeをスキップする
- extract（src/extract.rs）はdom_smoothieで本文・title・公開日時を抽出。--raw指定時はスキップし、代わりにconvertの`strip_non_content`で`<script>`/`<style>`/`<noscript>`だけを除去する（JSコード・CSSの本文混入を防ぐ）。一覧/インデックスページなど本文抽出が向かないページは--raw（JS描画なら--render併用）を使う
- convert（src/convert.rs）はhtmdでMarkdown化。クリックでスクリプトが走りうる実行系スキーム（javascript:・vbscript:・data:text/html・data:image/svg+xml）のリンク先は`unsafe-`接頭辞で無害化する。通常URLや非実行データURL（data:image/png等）はそのまま残す
- budget（src/budget.rs）は--start-index/--max-charsで文字スライスし、tiktoken-rsで出力スライスの概算トークン数を計測（--no-tokens時は省略）。切り詰め発生時は自己記述フッタを付与
- output（src/output.rs）は形式（markdown / frontmatter / json / text / html）に整形。本文はプロンプトインジェクション緩和として、端末制御文字を除去し、webgrab自身の制御マーカー`[webgrab:`の偽造を`[quoted-webgrab:`へ無害化する。完全防御はツール単体では不可能で、消費側エージェントの権限分離・自動実行禁止・人間確認が前提（信頼モデルはREADME/SKILL参照）

### モジュール構成

```text
src/
├── main.rs      # エントリ、終了コード変換のみ
├── cli.rs       # clap定義
├── error.rs     # WebgrabError（手書きのError/Display実装）と終了コード対応表
├── netguard.rs
├── robots.rs
├── fetch.rs
├── render.rs
├── render/
│   ├── wait.rs      # InFlight・should_stop・effective_cap・exceed_msg等の純関数
│   ├── world.rs     # 分離ワールドでの評価（IsolatedWorld）
│   └── intercept.rs # Fetch interceptとNetwork監視、共有状態、netguard遮断の集計
├── renderproxy.rs
├── renderproxy/
│   └── hostcache.rs # HostCache（ホスト解決2秒上限、netguard判定、実行単位キャッシュ）
├── decode.rs
├── extract.rs
├── convert.rs
├── budget.rs
└── output.rs
```

## 5. CLI仕様

```text
webgrab <URL> [OPTIONS]
```

| フラグ | 型 | デフォルト | 挙動 |
|---|---|---|---|
| `<URL>` | 必須位置引数 | なし | http/httpsのみ受理。それ以外は終了コード2 |
| --format | markdown \| frontmatter \| json \| text \| html | markdown | 出力形式（6章） |
| --max-chars | usize | 24000 | 本文（変換後Markdown）の最大文字数の上限。Claude CodeのBash 30,000文字制限の内側。0を指定した場合はメタデータヘッダのみ出力し終了コード0。切り詰め位置は範囲後半の最後の改行の直後まで戻すため出力が上限より短くなることがある。フッタ・`chars`・継続コマンドの`--start-index`は実際の終端を指す |
| --start-index | usize | 0 | 本文の開始文字オフセット（続き取得用）。総文字数以上を指定した場合は空本文+ヘッダ+終端フッタ`[webgrab:end total N chars]`で終了コード0 |
| --render | フラグ | off | chromiumoxideでJSレンダリング後のDOMを取得 |
| --auto-render | フラグ | off | 静的取得の可視テキスト（スライス前）が空または200文字未満のとき、同一プロセス内でJSレンダリングに切り替える。`--raw`でも同じ基準で切り替える（`--raw`の終了コード6・short-content免除は維持）。切替・失敗・skipはstderrの`info=`・`warn=`行、`render_status`、markdown/text/htmlの`[webgrab:render-status ...]`行で通知する。`--render`と同時指定時は`--render`が優先（stderr注記なし）。注意: 外部ページは本文を短くするだけでChrome起動を誘発でき、エスカレーション時は対象オリジンへ描画1ページ分（サブリソース含む）の要求が追加で発生する。自側のコストは残余`--timeout`と残余`--max-bytes`（両経路とも展開後バイト）で上限づける。text/plainはこのエスカレーション判定の対象外で、`render_status`は`static`のままになる |
| --wait-ms | Option\<u64\>（未指定時は5000） | 5000 | `--render`/`--auto-render`のrender時、`goto`開始からDOM取得までの上限ミリ秒（ナビゲーション時間を含む）。ネットワーク静止・DOM安定・可視テキスト200文字以上を満たせば上限前に終了する。`--auto-render`時は残余`--timeout`に丸める。`--render`と`--auto-render`のどちらも指定されていない場合は無視しstderrに`warn=flag-ignored flag=--wait-ms`を出す |
| --no-sandbox | フラグ | off | Chromeのsandboxを無効化する。起動直前にstderrへ`warn=no-sandbox`を1行出す（エスカレーションしない実行やskipされた実行では出さない）。継続コマンドにも再現する（用途はsandboxが起動しないCI環境に限り、通常利用では指定しない。「セキュリティと信頼モデル」参照） |
| --raw | フラグ | off | 本文抽出をスキップしページ全体をMarkdown化。--renderとの併用可 |
| --timeout | u64（秒） | 30 | 全体予算。静的経路は接続+読み取り、--render経路はChrome起動〜DOM取得までを含む上限 |
| --no-robots | フラグ | off | robots.txt確認をスキップ |
| --allow-private | フラグ | off | netguardの内部アドレス拒否を解除 |
| --no-tokens | フラグ | off | Tokensヘッダの計測・出力を省略 |
| --fence | フラグ | off | 本文を非信頼コンテンツフェンス`[webgrab:untrusted-content ...]`〜`[webgrab:untrusted-content-end]`で囲む（プロンプトインジェクション緩和）。jsonは本文がフィールド分離済みのため対象外 |
| --user-agent | String | `webgrab/<version> (+https://github.com/okamyuji/webgrab)` | UA上書き |
| --max-bytes | u64 | 20971520 (20MiB) | 取得データの上限。静的経路は圧縮転送の展開後バイト数に対しストリーミング読みで適用し超過時点で中断（終了コード4）。--render経路は`Network.dataReceived.data_length`（展開後）の合計を主判定とし、`page.content()`直前のDOM長評価（JSでネットワークを経ずに膨らませたDOM対策）を加えて、超過時点で終了コード4とする。SSRFプロキシ（第二層）のワイヤバイト計上は同じ上限で第二の保険として残す（`--auto-render`時は`reason=max-bytes`で静的結果へ復帰） |
| -o, --output | パス | なし（stdout） | ファイル出力。書き込み失敗は終了コード1 |

- stdoutには本文のみ、診断・警告・進捗はstderrのみに出す
- stderrのエラー・警告・情報の先頭行は機械可読の固定書式とする（`error=` / `warn=` / `info=`の3種。例: `webgrab: error=http status=503 retryable=true`、`webgrab: warn=short-content chars=42 hint=--render/--raw`、`webgrab: info=auto-render reason=empty chars=0`）。`info=`は失敗を意味せず、失敗する実行では`info=`の後に必ず`error=`ブロックが続く。「先頭行」とは各メッセージブロック（`webgrab: `で始まる行とそれに続く詳細行）の1行目を指し、プロセスのstderr全体の1行目ではない（`warn=`や`info=`が`error=`に先行してよい）
- 抽出後の本文が1文字以上200文字未満の場合、stdout本文の末尾に自己記述行`[webgrab:short-content 42 chars — if unexpected, retry with <提案>]`を付け、stderrにも同内容の警告を出す（終了コードは0）。提案は`render_status`で決める。`static`/`skipped`なら`--render/--raw`（JS描画ページか一覧ページの可能性）、`rendered`/`failed`/`no-gain`なら`--raw`（抽出が一覧等を落としている可能性。`--render`明示時も`rendered`扱い）。抑制せず通知する。0文字の場合は7章のとおり終了コード6。text/plainは`--raw`と同じく本節の対象外で、この判定もマーカーも出ない

### 終了コード表（--helpに全掲載）

| コード | 意味 | stderr先頭行の`error=`トークン |
|---|---|---|
| 0 | 成功 | （なし。警告時は`warn=`） |
| 1 | 内部エラー（バグ相当・出力ファイル書き込み失敗を含む） | internal |
| 2 | 引数・URL形式エラー（clap既定と一致） | usage |
| 3 | ネットワーク失敗（DNS・接続・タイムアウト・TLS・リダイレクトループ等のトランスポート層全般。リトライ可） | network |
| 4 | HTTPエラー（4xx/5xx）、サイズ超過、非HTMLコンテンツ。5xx/429は`retryable=true`を併記 | http |
| 5 | robots.txtによる拒否 | robots |
| 6 | 本文が空（抽出結果0文字） | empty |
| 7 | JSレンダリング失敗（Chrome未検出・起動失敗・CDPエラー・renderタイムアウト） | render |
| 8 | 内部アドレス拒否（netguard。--allow-privateで解除可能） | netguard |

stderr先頭行は`webgrab: error=<トークン> ...`の固定書式で、`error=`値は上表の空白を含まないトークンに限る。詳細（robotsの対象ルール、netguardの解決IP等、空白を含みうる情報）は2行目以降に出す。ただし終了コード6だけは書式が異なり、先頭行は`webgrab: error=empty hint=<提案>`（提案は`--render/--raw`または`--raw`。「短文提案の規則」欄参照）とし、人間可読メッセージ`empty body extracted; retry with <提案>`は先頭行に含めず2行目の先頭に出す（`hint=`トークンは短文警告の`hint=`と対称）。他の終了コードの`webgrab: error=<token> <message>`書式は変えない。

注記: 03-research-report.mdのD節に記載した暫定割当（6=予算超過）は本表で置換した。終了コードの正はこの表である。

## 6. 出力形式仕様

### markdown（デフォルト、Jina Reader互換）

```text
Title: <ページtitle>
URL Source: <最終URL（リダイレクト後）>
Published Time: <ISO8601、抽出できた場合のみ>
Tokens: <o200k_base概算（出力スライス分）> (chars: <出力文字数> / total: <総文字数>)

Markdown Content:
<本文Markdown>
```

`URL Source`はリダイレクト後の最終URLである。`--render`または採用された`--auto-render`（`render_status=rendered`）では、render後URL（`location.href`、httpまたはhttpsのみ、userinfoを除去、8192バイト以下）になる。それ以外の経路では静的経路の`final_url`のままである（詳細は10-fetch-fidelity-design.md §4.2）。

切り詰めが発生した場合は末尾に次の1行を付ける。終端は範囲後半の最後の改行の直後まで戻すため、フッタの範囲表記は実際の終端を指す（10-fetch-fidelity-design.md §4.4）。継続コマンドの生成規則は次のとおり。(1) --start-index以外の非デフォルトフラグをすべて再現する（--renderや--formatが欠けると続きのオフセット基準が変わるため）。例外: `render_status`が`rendered`なら`--auto-render`を`--render`に置換し、それ以外（`static`/`no-gain`/`failed`/`skipped`）なら`--auto-render`とrender系フラグ（`--wait-ms`、`--no-sandbox`、`--chrome-path`）を省略して静的経路を再現する（`--render`と`--auto-render`の同時指定では`--render`を1回だけ出す）。(2) --start-indexは再現ではなく新オフセットに置換する。(3) -o/--outputは再現対象から除外する（同一パス上書きによる部分結果の喪失を防ぐため、継続はstdoutに出す）。(4) `rendered`のときは置換後の`--render`と同じ環境設定で再現できるよう`--chrome-path`と`--no-sandbox`も再現する。値付きフラグ（`--user-agent`、`--chrome-path`）はURLと同じ`shell_quote`で囲む。

```text
[webgrab:truncated chars 0-24000 of 83000 — continue: webgrab <URL> --render --start-index 24000]
```

切り詰めなし（最終ページを含む）の場合はフッタを付けない。「フッタ不在が終端の合図」であることを契約とする。加えて、--start-indexが総文字数以上の場合のみ、明示の終端フッタ`[webgrab:end total 83000 chars]`を付ける（空本文でも読者が終端と判別できるようにするため）。--start-index末尾超過と--max-chars=0が同時指定された場合は終端フッタを優先する。

### frontmatter

同じメタデータをYAML front matter（`---`区切り、キー: title, url, published_time, tokens, chars, total_chars, truncated）にして本文Markdownを続ける。切り詰め時はtruncated: trueに加え、markdownと同じフッタ行を本文末尾に付ける。

### json

`{"title", "url", "published_time", "tokens", "chars", "total_chars", "truncated", "ended", "continue_command", "short_content", "untrusted", "untrusted_note", "render_status", "static_chars", "rendered_chars", "markdown"}` のFirecrawl風エンベロープ（1行JSON）。`untrusted`は常にtrue、`untrusted_note`は`markdown`が非信頼の外部データである旨の短い説明で、構造化消費者へも非信頼シグナルを渡す（テキスト形式の`--fence`に相当）。切り詰め時はtruncated=trueかつcontinue_commandに継続コマンド全文を入れる。終端（--start-index末尾超過）はended=true・markdown空文字列・truncated=falseで表す。--max-chars=0はmarkdown空文字列・total_chars入りで表す（total取得用途）。--no-tokens時もtokensキーはnullにするがchars/total_charsは常に保持する。`render_status`（`static`/`rendered`/`failed`/`no-gain`/`skipped`）はfrontmatterにも同じキーで加える。`--render`明示時は`rendered`、既定の静的経路は`static`。`static_chars`と`rendered_chars`（可視テキスト長）は、`static_chars`が静的フェーズを実行したら常に値（`--render`明示時のみnull）、`rendered_chars`がrenderを実行してDOMを得たときのみ値（`rendered`/`no-gain`、および`--render`明示時。`static`/`skipped`/`failed`ではnull）を持つ。text/plainでは抽出HTMLが存在しないため、`static_chars`は本文の文字数になる。

### text / html

textはMarkdown変換のかわりにタグ除去テキスト、htmlは抽出後（--raw時は取得まま）のHTMLを出す。メタデータヘッダは付けない。text/plainはこのhtml形式の定義の例外で、抽出やタグ除去を経ないため、既定・--raw・text・htmlのどの形式でも同じ素通しテキストを出す。切り詰め時はmarkdownと同じ`[webgrab:truncated ...]`行を末尾に付ける（htmlではHTMLコメントとして付ける）。終端フッタ`[webgrab:end ...]`も同様に末尾（htmlはコメント）に付ける。--max-chars=0はtext/htmlでは本文が空になるため、`[webgrab:meta-only total N chars]`の1行（htmlはコメント）を出しstderrにも注記する。ただしこの1行の直後に`[webgrab:render-status ...]`行が続きうる（`render_status`が`failed`/`no-gain`/`skipped`のとき）ため、text/htmlのstdout末尾が必ず1行だけとは限らない。

### render_status行と`[webgrab:render-status ...]`

JSONエンベロープとfrontmatterに`render_status`（`static`/`rendered`/`failed`/`no-gain`/`skipped`）と、可視テキスト長`static_chars`/`rendered_chars`を追加する。markdown/text/htmlでは`render_status`が`failed`/`no-gain`/`skipped`のときだけ、既存マーカーと同じ書式の`[webgrab:render-status <status> reason=<token>]`（`reason`は`failed`が`render|max-bytes|extract`、`skipped`が`timeout|max-bytes`、`no-gain`が`shorter`）を1行付ける（htmlはコメント。本文側の閉じ忘れ`<!--`にマーカーが飲み込まれないよう、マーカー群の直前に`-->`を1つ出す）。位置は`--fence`の閉じ行の外側で、出力順は「本文（フェンス内） → フェンス閉じ → `[webgrab:truncated ...]`または`[webgrab:end ...]` → `[webgrab:short-content ...]` → `[webgrab:render-status ...]`」に固定する。webgrab自身が生成する行なので偽造無害化の対象外（本文側の同名文字列は従来どおり`[quoted-webgrab:`になる）。--max-chars=0でもこの行は出す。`static`/`rendered`では行を増やさない。`-o`でファイル出力した場合はstdoutが空になるため、この行は出力ファイル側に入り、経路の判別はstderrか出力ファイルで行う。

### --no-tokensとchars/totalの保持

すべての形式で、--no-tokensはトークン計測のみを省略し、chars/total（markdown/frontmatterヘッダの`(chars: X / total: Y)`、jsonのchars/total_chars）は常に出力する。これにより終端判定がトークン計測の有無に依存しない。

## 7. エラーパスの挙動定義

| 事象 | 挙動 |
|---|---|
| DNS失敗・接続拒否・タイムアウト・TLS証明書エラー・リダイレクトループ | stderrに機械可読1行、終了コード3。名前解決(getaddrinfo)自体も--timeoutで打ち切り、無応答DNSでのハングを防ぐ |
| HTTP 4xx/5xx | stderrにステータスと1行説明（5xx/429は`retryable=true`）、終了コード4。403はメッセージ末尾に`hint=--render`を付ける（先頭行の書式と終了コード4は不変。自動エスカレーションはしない） |
| Content-Typeが text/html・application/xhtml+xml・text/plain 以外 | 終了コード4（メディアタイプは大文字小文字を無視して判定）。text/plainは抽出とMarkdown変換を通さず素通しする。変更点は、危険リンクスキーム（インラインリンク・オートリンク・参照定義の3形）の無害化、端末制御文字の除去、webgrab制御マーカーの無害化に限り、生のHTMLタグは変更しない |
| 本文HTMLの要素ネストが深すぎる（抽出処理が計算量爆発する水準） | 抽出前に線形の深さ判定で打ち切り、終了コード4（stderrに--raw提案） |
| --max-bytes超過（展開後バイト数、ストリーミング判定） | ダウンロード中断、終了コード4 |
| 文字コード判定失敗 | chardetng推定で強制デコード（置換文字許容）、stderrに警告、継続 |
| 本文抽出0文字 | stderrに`hint=`トークン付き提案（`render_status`が`static`/`skipped`なら`--render/--raw`、`rendered`/`failed`/`no-gain`なら`--raw`）、終了コード6。`--raw`とtext/plainは抽出自体を行わないため対象外で、空本文でも終了コード0 |
| robots.txt disallow | stderrに対象ルール、終了コード5（--no-robotsで回避） |
| robots.txt自体の取得失敗 | 許可とみなし継続（相場どおり）、stderrに注記 |
| 内部アドレス着地（初回またはリダイレクト先） | stderrに`host=<host> resolved=<ip> range=<range>`（静的経路・render経路とも同じ書式。名前解決失敗は`resolved=unresolved`、2秒の解決上限超過は`resolved=timeout`で`range=`は省く）、終了コード8（--allow-privateで回避） |
| Chrome未検出/CDP失敗/renderが--timeout超過 | 終了コード7、stderrに静的取得コマンド提示 |
| `--auto-render`のエスカレーション時、renderフェーズの終了コード7・`--max-bytes`超過、render後の抽出失敗 | stderrに`warn=auto-render-failed reason=<render\|max-bytes\|extract>`と詳細行、静的結果を採用（`render_status=failed`）。終了コードは静的結果の長さに従う（空なら6、短文なら0+short-contentマーカー）。終了コード8だけは静的結果も出さず出力なしで伝播し、他のどの失敗分類よりも優先して判定する |
| クロスホストリダイレクト | 追従する（最大10回）。各ホップでnetguard（スキーム・IP）を再適用し、最終ホストのrobots.txtを確認する。最終URLをURL Sourceに記録 |

## 8. テスト戦略

- カバレッジ: cargo-llvm-covで計測する。機械判定の合格線は`--fail-under-lines 80`。coverageジョブはE2E（実Chrome）を含めて計測し、render.rsの除外はしない（従来はChrome依存のため既定計測から除外していたが、`--auto-render`の中心実装であるrender.rsとpipelineのrenderフェーズを計測対象に入れないと閾値が新規コードを見ないため解除した）。80%未満で合格した場合は`--ignore-filename-regex 'render\.rs'`を戻して達成するか、差分をIMPROVEMENT_BACKLOG.mdに記録する
- 単体テスト: netguard（プライベート/リンクローカル/ループバック/パブリックの判定）、decode（Shift_JIS/EUC-JP/UTF-8/charset無しの4ケース）、extract（記事系・空本文、F2のプレースホルダmarkup）、convert（見出し・コード・リンク・表、`visible_text_len`）、budget（境界: start-index=0/中間/末尾超過、max-chars=0/巨大、マルチバイト境界）、robots（Disallow/Allow/ワイルドカード/UA別/取得失敗）、render/wait.rs（`InFlight`・`should_stop`・`is_main_navigation`等の純関数）、pipeline（`escalation_reason`・`remaining_budget`・`choose_result`・`fallback_reason`・`hint_for`）、cli（`extra_flags`の置換・省略規則）、output（`render_status`・`static_chars`/`rendered_chars`のnull規則）、error（先頭行トークン付与、詳細行のサニタイズ）、budget（切り詰め終端を改行境界へ調整する規則）、fetch（`http_error_message`・`is_plain_text`）、decode（HTML metaの走査を省く経路）、render（`resolve_final_url`）、convert（`sanitize_link_schemes`の3形）、pipeline（`plain_stage`）
- 統合テスト: tests/配下でstd::net::TcpListenerの最小HTTPサーバを立て、実HTTP経由でCLIバイナリを起動して終了コード・stdout/stderr分離を検証する。統合テストはcargo llvm-cov経由で実行し、子プロセスのprofrawが合流されカバレッジに計上されることを実装フェーズ最初に確認する
- E2Eテスト（実Chrome、tests/render_e2e.rs）: ローカルfixture（最小HTTPサーバ + 実Chrome）でF2の待機戦略、`--auto-render`のエスカレーション・no-gain・skip、`--max-bytes`超過（gzip fixture・DOM膨張fixture）、内部アドレス到達（終了コード8）を検証する。`WEBGRAB_E2E=1`で有効化し、未設定かつ`CI`未設定ならskip、未設定かつ`CI`設定済みなら失敗とする。詳細は08-js-render-design.md §6
- smoke test（examples/smoke.rsで実施済み、2026-07-17）: (1) chromiumoxide 0.9.1でChrome 150とCDP接続しexample.comを559バイト取得できた。なおtokio-runtimeという名のfeatureは存在せず、既定featureのままtokioで動作した（設計の必須feature指定は不要と判明） (2) htmdはtableをMarkdownテーブルへ変換できた（`| A | B |`形式、素通しフォールバック不要） (3) dom_smoothieはpublished_timeフィールドを取得できた（metaタグ自前抽出フォールバック不要）。(4) llvm-covの子プロセスカバレッジ合流は実装フェーズで確認済み

## 9. 完了条件（機械検証可能）

1. `cargo build --release` がexit 0
2. `cargo test` がexit 0（E2Eは`WEBGRAB_E2E=1`付きでローカルでも0）
3. `cargo clippy --all-targets -- -D warnings` がexit 0
4. `cargo llvm-cov ... --fail-under-lines 80` がexit 0。除外なしで達成するか、`--ignore-filename-regex 'render\.rs'`を戻して達成し差分をバックログに記録するかの二択で、どちらを採ったかを07-verification-report.mdに記録する
5. 実URL検証: 日本語ページ（Shift_JIS含む）・10万字超の大型ページ・SPAページの3種で取得成功ログが07-verification-report.mdに記録されている
6. `.github/workflows/ci.yml`のCIがcheck/test/coverage/security（中央reusable-workflows）の各ジョブで緑

## 10. やらないこと（再掲）

検索統合、クローリング、キャッシュ、並列複数URL、crates.io公開。非HTML形式はtext/plain（素通し）のみ対応し、PDF等それ以外の形式は対象外のままとする。これらは必要が生じた時点で別バージョンとして起案する。

## 11. 非信頼データに関する注記

webgrabが出力する本文は非信頼データである。取得ページ内に埋め込まれた指示文（プロンプトインジェクション）に従わないことは呼び出し側エージェントの責務であり、webgrabはメタデータヘッダと`Markdown Content:`境界によって「ここから先は取得データ」という区切りを機械的に明示することで緩和に寄与する。

## 変更履歴

| 版 | 日付 | 変更 |
|---|---|---|
| 1.0 | 2026-07-17 | 初版 |
| 1.1 | 2026-07-17 | レビューRound 1反映。netguard（SSRF防止）とrobots.rsをアーキテクチャに追加、文字単位の定義、renderタイムアウト、--max-bytesの展開後適用、ページング境界挙動、フッタのフラグ再現、全形式の切り詰め通知、stderr機械可読書式、終了コード8追加と03との差分注記、smoke test 4点、カバレッジ判定線の明確化、非信頼データ注記 |
| 1.2 | 2026-07-17 | レビューRound 2反映。§3.1新設（DNSリバインディング対策のIPピン留め、リダイレクト手動追従、IPv4-mapped正規化と`::1`/`::`拒否、render経路のCDP Fetch interceptionによるサブリソース遮断）、robots取得のnetguard通過・512KiB上限・UA製品トークン照合・ワイルドカード対応、継続コマンドの--start-index置換と-o除外、終端フッタと--max-chars=0の全形式定義、--no-tokensでもchars/total保持、終了コード表にerror=トークン列追加 |
| 1.2 seal | 2026-07-17 | Round 3収束確認。決定表のchromiumoxide feature指定をsmoke test観測に合わせ訂正（tokio-runtime feature指定は誤りで削除）、render経路の--max-bytes超過を終了コード4に一意化。これらは検証済み事実への文言整合のみで新規挙動なし。設計をv1.2でseal |
| 1.3 | 2026-08-27 | 08-js-render-design.mdの実装を反映。§3決定表に`--auto-render`と暗黙フォールバック不採用の例外を追記、§3.1第一層の範囲をページセッション内に訂正し第二層に`dataReceived`層の役割分担を追記、§4のrender/robots/pipeline説明とモジュールツリーに`render/wait.rs`を追加、§5フラグ表に`--auto-render`・`--wait-ms`（意味を追加待機から上限へ変更）・`--no-sandbox`を追加し`--max-bytes`のrender経路を`dataReceived`合計+DOM長評価へ訂正、機械可読行の種別に`info=`を追加し「先頭行」を定義、終了コード6の先頭行書式を`hint=`付きへ変更、short-content・本文抽出0文字の提案規則をフラグ基準から`render_status`基準へ変更、§6にjson/frontmatterの`render_status`・`static_chars`・`rendered_chars`と`[webgrab:render-status ...]`行、継続コマンド規則に`--auto-render`の置換・省略規則と`--no-sandbox`再現・`shell_quote`を追加、§7に`--auto-render`失敗時の挙動と終了コード8詳細行の書式統一を追加、§8をE2E戦略とカバレッジ80%・render.rs除外解除へ更新、§9完了条件を更新 |
