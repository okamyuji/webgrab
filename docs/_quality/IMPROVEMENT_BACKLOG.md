# 改善バックログ

## 設計レビューRound 4（2026-08-27、docs/08-js-render-design.md v1.4、N=3: 同一視点。ユーザー承認による最終ラウンド）

判定はReviseで（3名とも）、Round 3所見は全件ADDRESSED、新規所見はblocking15件・non-blocking24件（重複含む）だった。ユーザー指示により各所見を一次情報（コード・crateソース・probe3/4）で検証し、v1.5 sealで根本原因を解決した。v1.5は独立再レビューを経ていない（上限）。

### 一次情報で正当と確認しv1.5で根本対処した所見

- [security] 展開後超過の中断が`main_blocked`を捨てる → `render_inner`が`drive`の結果によらず`main_blocked`を先に評価する単一経路
- [DoS] `--max-bytes`がDOM膨張と`page.content()`を有界にしない（chromiumoxide `conn.rs`は`max_message_size(None)`） → `content()`前に分離ワールドでDOM長を1回評価し残余超過は終了コード4。dom_bomb fixture/E15
- [contract] OOPIF/Service Workerは第一層と`dataReceived`の外（probe3で実測。`target.rs`はSWをdetach） → 主張を訂正し限界(d)を定量化。子セッションへの`Fetch`/`Network`付与は次版候補
- [security] 直列interceptの遅延が攻撃者制御下で終了コード8が0に落ちる → interceptの個別タスク化（同時16）+ 実行単位のホスト判定キャッシュ（プロキシと共有）+ `warn=netguard-blocked layer=`通知
- `main_frame_id`の取得時期と失敗時が未定義（`mainframe()`は`Result<Option<_>>`） → `Fetch.enable`前に取得、`Err`/`None`は終了コード7
- `fallback_reason(err)`が終了コード4の2事象を区別できない（`extract.rs`/`render.rs`とも`ExitCode::Http`） → `fallback_reason(phase, err)`
- big_gzip fixtureの供給元が無い（dev-dependencies無し） → `tests/fixtures/big_gzip.html.gz`をコミット、`tests/common`はバイト本文と任意ヘッダ対応
- `value_source`は`main.rs`で`ArgMatches`を破棄しているため使えない → `--wait-ms`を`Option<u64>`化。`shell_quote`は`pub(crate)`
- render予算がChrome起動を含み予備2000msが残らない → deadline基準の実効wait、skip閾値5秒（§8で実測して見直し）
- §4.5の04更新リストがv1.4の中核（04 §5:122、§3.1:58/60、§8:204、§6:179）を落とす → 追加
- 512バイト切り詰めのUTF-8境界、htmlの閉じ忘れ`<!--`、skip統合テストの入力条件、`visible_text_len`テスト期待値、E2の余裕、`ubuntu-latest`の移動標的、YAML内のSHAピン留めと`persist-credentials`と機械検証、SKILL終了コード表の6行、README文言、`InFlight`の共有方法（`Arc<Mutex>`）、probe(9)の実測条件、`static_chars`規則の明記、静的消費バイトの供給元（`Fetched`）

### 検証の結果、所見の前提が成立しなかったもの

- `data:`/`blob:`が`dataReceived`から漏れる（UNVERIFIED所見） → probe3で計上されることを確認（§1 probe(10)）
- `Swatinem/rust-cache`の大小 → SHAピン留めで表記自体が置換される

### 実装前probe補遺（2026-08-27、v1.6）

- `page.content()`はメインワールドで`outerHTML`を評価するため、ページ側のgetter上書きで偽装できる（probe(14)、3バイトの偽値を観測）。DOM取得を分離ワールドの評価に変更した。既存の`--render`（v1.2以前）はこの偽装に対して無防備だった
- 分離ワールドは`about:blank`時点で作るとナビゲーションで破棄される（probe(13)） → `goto`後に遅延生成

### 未採用（次版候補として記録）

- OOPIF/Service Workerの子セッションへ`Fetch`/`Network`を付与して展開後計上と第一層遮断を広げる（`Target.setAutoAttach`はchromiumoxideが既にflattenで発行。子targetへのコマンド発行経路の調査が必要）。`--disable-features=site-per-process`はprobe4で効果を観測できず未確認
- `--max-chars 0`でのエスカレーションを`skipped reason=meta-only`にする案（現状は「renderする+行を出す」で確定）
- サブリソースのrobots未照合 → `--render`単体と同じ既知制約
- 250msポーリング分の展開後超過（上限+1間隔の受信量） → §4.2に明記済み

## 設計レビューRound 3（2026-08-27、docs/08-js-render-design.md v1.2、N=3: 同一視点）

判定はRevise（3名とも）で、Round 2所見は全件ADDRESSED、新規所見はblocking11件・non-blocking43件（重複含む）だった。ラウンド上限（3回）に達したため、blocking所見と安価なnon-blocking所見をv1.3に反映したが、v1.3自体は独立再レビューを経ていない。

### blocking所見 → v1.3で対応済み（再レビュー未実施）

- [security] 終了コード8とrenderフェーズ`--max-bytes`超過の優先順位が未定義で、実装は4が8を上書き → 8を最優先で判定
- [data-loss] `main_blocked`がiframeの`Document`要求でも立ち、ポーリング再確認で任意ページが終了コード8を強制できる → メインフレーム限定（`is_main_navigation`）
- `remaining_budget`の署名がskip理由を返せない → `Result<_, SkipReason>`
- `convert::to_text`はリンク先URLを含み、ナビ付き空シェルでエスカレーションが発火しない → `visible_text_len`を新設。short-content/終了コード6との非対称は明記
- `wait_for_navigation`が待機予算を使い切りうる → 上限`min(1000ms, 残り)`
- 判定用`to_text`の失敗が未定義 → 失敗しない関数に置換
- `goto`が`Err`のとき`main_blocked`未反映で終了コード7になりE9が不安定 → 同期待ち+再確認
- `no-gain`の`reason`が未定義 → `reason=shorter`
- §9がE12を含まず番号が壊れている → 修正
- §4.5の04更新リストがshort-content提案規則（04 §5:127）と§7:195を落としている → 追加
- `[webgrab:render-status ...]`行のフェンス内外と他マーカーとの順序が未定義 → フェンス外、順序固定

### non-blocking所見のうちv1.3で対応したもの

`page.content()`のタイムアウトと予備2000ms、`saturating_sub`、非エスカレーション時のrender系フラグ省略、`shell_quote`、`value_source`、`hint_for`のタプル化、`RenderStatus`の定義位置、`fallback_reason`、tombstoneのTTL/上限、限界(f)(g)、終了コード8詳細行に層とIP、skip契約の統合テスト、E7の許容緩和、E11の終了コード、fixture本文長、`static_chars`/`rendered_chars`の規則、`--max-chars 0`でのrender-status行、frontmatterの言及、配置済みSKILLコピーの反映手順、actionのSHAピン留めと`persist-credentials: false`、coverageのChrome確認、skip閾値の実測前提

### Round 3所見の一次情報検証（2026-08-27、v1.4）

Round 3のblocking所見を実コード・crateソース・probeで検証した。終了コード8の上書き（`render.rs`の`exceeded()`判定順）、iframeでの`main_blocked`（probeで別`frame_id`を確認）、メイン文書遮断時の`goto` `Err`（probe）、`to_text`のURL残存（`convert.rs`）、`--user-agent`の素朴なクォート（`cli.rs`）、`wait_for_navigation`のload待ち（`target.rs`）はすべて正当。gzip爆弾は`Network.dataReceived.data_length`が展開後バイトを返すことをprobeで確認し、v1.4でrender経路の主判定に採用して根本対処した（ユーザー指示）。

### 未採用（次版候補として記録）

- `--max-chars 0`でのエスカレーションを`skipped reason=meta-only`にする案 → 現状は「renderする+行を出す」で確定
- サブリソースのrobots未照合 → `--render`単体と同じ既知制約。SKILLに注記
- interceptの同期待ちは処理中イベントのみ保証（未受信は対象外） → 限界(e)として明記済み
- E2Eで終了コード8伝播（`--auto-render`）を検証できない（`--allow-private`が必要） → `fallback_reason`の単体テストで担保
- testとcoverageでE2Eを二重実行 → 許容と明記
- 明示`--wait-ms 5000`の`flag-ignored`判定 → `value_source`で対応済み

## 設計レビューRound 2（2026-08-27、docs/08-js-render-design.md v1.1、N=3: 同一視点）

判定はReviseで、新規所見はHigh11件・Medium24件・Low17件（重複含む）だった。Round 1所見は3名とも全件ADDRESSED判定。合意所見と特別カテゴリをv1.2で反映。

### 合意所見（K=2以上）→ v1.2で対応済み

- `Fetch.requestPaused`の`request_id`は`Network`の`RequestId`と別空間 → `network_id`をキーに削除（probeでも確認）
- render後のextract/convert失敗が未定義で静的結果を捨てる → `reason=extract`で静的へ復帰
- 「stderr先頭行」の契約とwarn/infoの先行順序が矛盾 → 先頭行＝ブロック1行目と定義、テスト期待を「行が含まれる」に変更、`info=`の自己矛盾文を訂正
- 残余`--max-bytes`のskip条件が0のみで超過時に静的結果を捨てる → skip閾値3秒/256KiB、renderフェーズの超過は`reason=max-bytes`で静的へ復帰、伝播は終了コード8のみ
- エスカレーション判定量が`--format`/`--raw`依存 → 可視テキスト長に統一。`--raw`の終了コード6/short-content免除は維持
- E9がrobots事前確認で落ちてrender層を通らない → `--no-robots`を付ける
- Ubuntu 24.04ランナーのsandbox問題と`WEBGRAB_NO_SANDBOX`の隠れトグル → CLI `--no-sandbox`（`warn=no-sandbox`、継続コマンド再現、README信頼モデル）、E2Eハーネス環境変数を表で明示、CIは最初から無効化
- 新規コードがカバレッジ対象外 → coverageジョブにE2Eを含め`render.rs`除外を解除。pipelineの判定を純関数に切り出し
- `--chrome-path`は既に継続コマンドに再現されている（Round 1所見が誤検出） → 記述を訂正
- E10が2実行分を1行に混在 → E10a/E10bに分割
- skip時のshort-content提案と終了コード6の`hint=`が未定義、`--render`明示時も`--render`を提案 → `hint`を`render_status`基準に統一
- SKILLの`hint=`規則の更新漏れ → §4.5に明記

### 単独所見のうち特別カテゴリ・技術的妥当性でv1.2に採用したもの

- [security] `main_blocked`の設定遅れ → 手順6でinterceptの同期待ち（最大500ms）、残る窓は限界(e)として明記
- [security] renderproxyの解決に上限が無い → 2秒上限を追加
- [contract] `--max-bytes`の計上単位が経路で異なる → §4.3/§4.4に明記
- [doc-contradiction] 終了コード8の伝播が「捨てない」原則と衝突 → 決定表に理由を明記（出力なしで中断を維持）
- 実効waitが残余予算に丸められず終了コード7へ倒れる → `min(--wait-ms, 残余−1秒)`
- stderr詳細行のサニタイズ規則が未定義 → 既存サニタイザ共有、改行畳み込み、512バイト上限
- 既定形式でフォールバックが不可視 → `[webgrab:render-status ...]`行（failed/no-gain/skippedのみ）
- `outerHTML`の毎回シリアライズはO(DOM) → 要素数+innerText長に変更、理由文を訂正
- 静的フェーズがエラーで終わった場合のエスカレーション可否 → 伝播
- 解決上限の根拠文（直列処理と両立しない） → 訂正
- tombstone、最短750ms、`evaluate_expression`、`..Default::default()`、超過メッセージに指定値併記、`no-gain`の意味と`static_chars`/`rendered_chars`、対象オリジンへの負荷注記、`--max-chars 0`の注記、workflowのpermissions/timeout/concurrency、csr_slowを2500msに

### 未採用（次版候補として記録）

- サブリソースが内部アドレスを参照するE2E → ローカルfixtureは`--allow-private`が必要で両立しない。render層の遮断はE9（メインナビゲーション）で担保し、サブリソース遮断は既存単体テスト（`unresolvable_host_is_fail_closed`等）に委ねる
- interceptの並列化・DNSキャッシュ・キャンセル可能リゾルバ → スコープ外
- 終了コード8時に静的結果を出力する案（B） → 既存契約維持（A）を選択。必要になれば別起案
- 04 §7「終了コード8はstderrに解決IPと対象レンジ」がrender経路で未実装（既存の不一致） → 04 v1.3更新時に整合させる（§4.5に記載）

## 設計レビューRound 1（2026-08-27、docs/08-js-render-design.md v1.0、N=3: 実装者/エージェント利用者・CI/セキュリティ運用視点）

判定Revise（Critical 1 / High 18 / Medium 32 / Low 25、重複含む）。K=2合意と特別カテゴリ（セキュリティ・データ喪失・契約不一致・文書矛盾）をv1.1で反映。

### 合意所見（K=2以上）→ v1.1で対応済み

- in-flight計数がリダイレクトで正に張り付く／取りこぼしで負になる → `RequestId`集合に変更
- `wait_for_navigation`の即時復帰／ハング → タイムアウト付き最善努力とし、正しさはポーリング条件に依存
- evaluateの失敗・`body`欠落・巨大文字列転送 → ページ内で数値だけを返す式、失敗は条件未達、各呼び出しにタイムアウト
- `extra_flags`の`--wait-ms != 2000`リテラル → 定数化
- `--auto-render`で`--timeout`が2倍 → 残余予算をrenderへ渡し、不足時はskip
- 静的とrenderで`final_url`が食い違う → 静的の`final_url`へ遷移・報告
- `--raw`との相互作用未定義 → `--raw`でも同基準でエスカレーション
- 空本文チェックがエスカレーションより先に走る → pipelineを3フェーズに再構成
- short-content提案が`cli.render`基準 → `render_status`基準
- E7の期待値が§4.1と矛盾 → fixture markupを固定し期待を明記
- エスカレーション判定の入力（全文かスライスか） → スライス前全文に固定
- `--max-bytes`が2重計上 → 残余をrenderへ渡す
- stderr `warn=... error=...`の契約違反・改行混入 → `reason=`トークン + 2行目詳細（制御文字畳み込み）
- `rendered`真偽値では状態を区別できない → `render_status`列挙、frontmatterにも追加
- robots「静的経路で確認済み」の非等価 → 制約として明記し04 §4も訂正

### 単独所見のうち特別カテゴリ・技術的妥当性でv1.1に採用したもの

- [security] フォールバックが終了コード8/4を飲み込む → 7に限定
- [data-loss] render結果が静的より短い場合の採用 → 長い方を採用、`no-gain`
- [security] `main_blocked`の再確認が待機ループ後に無い → 各ポーリングで再確認
- [contract] 終了コード6の`hint=`トークン不在 → 先頭行に追加
- [contract] 非エスカレーション時の`--auto-render`再現で別文書を切り出す → 省略
- [contract] `--chrome-path`が継続コマンドに無い → 再現対象に追加
- E2Eが`CI`下で静かにskipしうる → `CI`設定時は失敗
- E2E並列実行のflake → 直列化。csr_slowを3000msに
- `GrabFailed`以外の`parse`失敗を空本文に写像しない、`warn=extract-grab-failed`
- interceptのホスト解決に上限（2秒）
- カバレッジ閾値60は緩すぎる（実測88.6%） → 80
- CIを実YAMLで記述、tests/commonの`dead_code`、`Meta`の`Default`

### 未採用（次版候補として記録）

- 可視テキスト200文字以上の条件はナビ・フッタの長いSPAで即成立し、タイマー描画を検知できない。検知手段が無いため§4.2の限界(c)として文書化した。改善案: `--min-wait-ms`（最小待機）の追加、または静的経路の可視テキスト長との比較
- interceptハンドラの並列化とDNS結果キャッシュ → スコープ外（解決タイムアウトのみ採用）
- サブリソースが169.254.169.254を参照するE2E → ローカルfixtureは`--allow-private`が必要で両立しないため見送り
- render経路の続き取得の非決定性 → 既知の制約として明記のみ

## 設計レビューRound 1（2026-07-17、N=3: 実装者/利用者/安全運用視点）

### 合意所見（K=2以上）→ v1.1で対応済み

- SSRF防止（netguard、リダイレクトホップ再検証、終了コード8）: 3票
- 03と04の終了コード6の意味の食い違い: 3票 → 両文書に注記
- --render経路のタイムアウト未定義: 3票 → --timeoutを全体予算化
- 「文字」単位の未定義: 2票（実装者・利用者） → Unicodeスカラー値と半開区間を定義
- ページング/切り詰め契約の不完全（start-index末尾超過、max-chars=0、フッタのフラグ再現、全形式の切り詰め通知、Tokensの対象）: 2票クラスタ → 5章・6章で定義

### 単独所見のうち技術的妥当性を理由にv1.1で採用したもの

- --max-bytesの展開後適用（zip爆弾によるOOM=サービス停止リスク、安全運用視点のみ）
- robotsワイルドカード`*`/`$`対応+解釈不能時はdisallow側（安全側原則、利用者視点のみ）
- stderr先頭行の機械可読書式（利用者視点のみ。終了コード4の過積載の緩和策として採用）
- 本文0文字(exit 6)と200文字未満(exit 0)の境界衝突の解消（利用者視点のみ。契約内矛盾のため）
- robots.rs/netguard.rsのモジュール構成明記（実装者視点のみ。契約と構成の不一致のため）
- Chrome後始末のDropガード（安全運用視点のみ）
- 非信頼データ注記（安全運用視点のみ）
- smoke test 4点（実装者視点の未検証API群: chromiumoxide tokio feature、htmd表、dom_smoothie公開日時、llvm-cov子プロセス合流）

### 未採用（次版候補として記録）

- 終了コード4を4xx/5xx/非HTMLで別番号に分離する案（利用者視点）。v1.1ではstderr機械可読書式で代替。エージェント側の判別が実運用で不足したら改番を検討
- --max-tokensによるトークンベース切り詰め（利用者視点）。v1.1では文字ベースのみと契約明記。需要が観測されたら追加
- 出力ヘッダへのprompt injection警告文の埋め込み（安全運用視点の派生案）。ヘッダ肥大とのトレードオフのため見送り

## 設計レビューRound 2（2026-07-17、修正後の再レビュー）→ v1.2で対応

### 合意所見（K=2以上）または技術的妥当性特別カテゴリ（単独Critical/High）→ v1.2対応済み

- DNSリバインディング/TOCTOU（C=Critical, A=High、特別カテゴリ）: §3.1でIPピン留めを契約化
- render経路のサブリソースSSRF迂回（C=High, A=High、特別カテゴリ）: §3.1でCDP Fetch interceptionを契約化
- robots取得のnetguard通過・サイズ上限・手動リダイレクト（C=Medium、A=Medium、合意）はrobots.rs仕様に反映した
- 継続コマンドの--start-index衝突（B=High, A=Low、契約と実装の不一致）: §6で置換規則を明記
- --max-chars=0/終端フッタの形式横断（B=High/Medium, A=Low）: §6で全形式定義
- -o継続コマンドの上書き（B=Low、A=Low、合意）は§6で-oを除外して解消した
- robotsのUA製品トークン照合規則（A=Medium、単独、契約完全性）はrobots.rs仕様に反映した
- IPv4-mapped IPv6正規化と`::1`/`::`拒否（C=Medium、単独、セキュリティ）は§3.1に反映した
- stderrのerror=トークン一覧（A=Low、単独）は終了コード表にerror=列を追加して解消した

### 未採用（次版候補）

- 終端の最終ページで「フッタ不在=終端」に頼る点（B=Medium）。v1.2で契約として明文化したが、明示シグナルを常時付ける案は出力肥大のため見送り

## 実装レビュー（2026-07-17、rust-reviewer）→ 対応済み

判定Block（CRITICAL 1・HIGH 3）。すべて修正しテスト緑を確認。

- C1 [CRITICAL] robots.txt取得がreqwest自動リダイレクトでnetguard未検証（SSRFバイパス）→ 手動1回追従+追従先resolve_checked再検証に修正。回帰テスト`robots_redirect_is_manually_followed_once`追加
- H1 [HIGH] resolve_checkedの同期DNS解決をasyncで直呼び→ spawn_blockingでラップ
- H2 [HIGH] extra_flagsが非デフォルトフラグを取りこぼし（継続コマンドの再現性）→ wait-ms/timeout/no-robots/max-bytes/user-agent/chrome-pathを追加
- H3 [HIGH] render.rsの一時user-data-dir生成・Drop削除が未実装→ tempfile::TempDirで生成しRAII削除。実機で残留なしを確認
- M1 render.rsのbuilder unwrap→ match+stderr警告に変更
- M2 budget.sliceのVec<char>全コピー→ char_indicesベースに変更（設計§3準拠）
- M3 main.rsの同期fs::write→ tokio::fs::write
- M4: 同一ホップでのDNS二重解決は、解決済みaddrをrobots_allowedへ渡して回避した
- M5 tokens.rsのexpect→ 妥当性コメント付与
- L1 tests/integration.rsのfmt差分→ cargo fmt適用
- L2 netguardのis_broadcast追加拒否→ 安全側の追加として記録のみ（変更なし）

## 実装敵対的レビュー Round 2（2026-07-18、複数rust-reviewer + セキュリティ視点）

判定Block。OWASP対応（A03:Injection / A10:SSRF）と確認済みバグを修正、各々に回帰テスト新設。全90テスト・clippy緑。

### 修正済み（テスト付き）

- [CRITICAL/A10] netguard: 6to4/NAT64/Teredoに埋め込まれた内部IPv4、及びマルチキャスト(224/4, ff00::/8)・予約(240/4)未拒否 → `embedded_v4`と判定追加。テスト`embedded_v4_transitions_denied`他
- [CRITICAL] robots: 同一UAの複数グループが非結合でDisallow無視（RFC 9309 §2.2.1違反）→ union実装。テスト`same_agent_multiple_groups_are_merged`
- [CRITICAL/A10] render: DNS解決失敗時fail-open → fail-closedに（fetchと対称）。テスト`unresolvable_host_is_fail_closed`他
- [CRITICAL] extract: 深いネストHTMLでdom_smoothieが3乗的にハング(DoS) → 線形の深さガード(上限1000)。テスト`deeply_nested_html_is_rejected_fast`
- [CRITICAL/A03] output: 本文中のESC/OSC等C0制御文字が素通しで端末インジェクション → `strip_terminal_controls`。テスト`body_terminal_escapes_stripped`
- [CRITICAL/A10] render: DNSリバインディングTOCTOU完全対処 → 検証・IPピン留めローカルプロキシ`src/renderproxy.rs`を新設し、`--proxy-server`+`--proxy-bypass-list=<-loopback>`でChromeの全接続（loopback含む）を経由させ、プロキシ側で解決→netguard判定→検証済みIPへ接続固定。Chromeに再解決させないため原理的にTOCTOUを閉じる。単体8テスト+実機検証（metadata宛exit 8遮断／example.com正常レンダリング）
- [HIGH/A03] output: 非信頼titleの改行でfrontmatter YAMLキー/markdown偽メタ行を注入 → `sanitize_line`+`yaml_scalar`。テスト`frontmatter_title_newline_injection_neutralized`他
- [HIGH/A03] budget: 継続コマンドのURL未クォートでコピペ時シェル誤動作/注入 → `shell_quote`。テスト`continue_command_quotes_url_with_query_string`
- [HIGH] output: `--max-chars 0`で自己参照する継続コマンド生成→LLM無限ループ → メタのみ時フッタ抑止。テスト`max_chars_zero_has_no_self_referential_continue`
- [HIGH] fetch: robots.txt取得が`resp.bytes()`でメモリ非上限(DoS) → ストリーミング上限`read_capped_robots`
- [HIGH] robots: percent-encoding非正規化で拒否対象を許可 → `percent_decode`で正規化。テスト`percent_encoded_path_normalized`
- [MEDIUM] decode: `<meta>`外の`charset=`(canonicalリンク等)を誤採用し本文文字化け → `<meta>`タグ内限定走査。テスト`charset_in_non_meta_tag_is_ignored`
- [MEDIUM] decode: BOM上書き後の返却エンコーディング名が実態と不一致 → `decode`の第2戻り値を返却。テスト`bom_overrides_declared_encoding_and_label_matches`
- [MEDIUM] fetch: Content-Type大小区別で`TEXT/HTML`を誤拒否 → `is_supported_media_type`で小文字化。テスト`media_type_check_is_case_insensitive`
- [MEDIUM] convert: `to_text`が本文先頭の`--`/`###`を誤除去しデータ欠損 → 記法プレフィックス限定除去+行頭バックスラッシュ解除。テスト`text_preserves_literal_leading_dashes`他

### 設計整合（2026-07-18、機能追加系）→ 対応済み（テスト+実機確認）

- [HIGH/DoS] render経路の`--max-bytes`適用 → SSRFプロキシで全接続のダウンロード総量を計上し超過を終了コード4に。実機確認（`--max-bytes 500`でexit 4）。設計§3.1・パラメータ表を更新
- [MEDIUM] 設計§5「短い本文」stdoutマーカー → 全形式で本文末尾に`[webgrab:short-content N chars — …]`を付与（jsonは`short_content`フィールド）。テスト`short_content_marker_appended_markdown_and_json`+ローカルサーバ実機確認（chars=98）
- [MEDIUM] 設計§7「文字コード判定失敗時のstderr警告」 → `decode`が`had_errors`を返し、pipelineが`warn=decode-replacement`を出力。テスト`invalid_bytes_report_had_errors`+実機確認（Shift_JISをUTF-8偽装で警告発火）
- [LOW] 未使用依存`thiserror`をCargo.tomlから削除、設計§4のerror.rs注記を「手書き実装」に修正。あわせてrenderproxyが直接使う`tokio`の`net`/`io-util` featureを明示追加
- 設計文書の整合: Content-Type大小無視・要素ネスト深さ上限（終了コード4）を§7異常系表に明記

### 残3点（2026-07-18、過剰にしない最小設計で対応）→ 対応済み（テスト+実機確認）

- [MEDIUM] `--render`時のrobots非対称 → `fetch::robots_precheck`を追加し、renderの前にトップURLのrobots.txtを確認（静的経路と同じ範囲、`--no-robots`でスキップ）。実機確認（全拒否robotsで`--render`→exit 5、`--no-robots`→exit 0）
- [MEDIUM/A03] 危険スキームリンク → `convert::sanitize_link_schemes`で`javascript:`/`vbscript:`/`data:text/html`/`data:image/svg+xml`のみ`unsafe-`接頭辞化。通常URL・`data:image/png`は不変。テスト`dangerous_link_schemes_neutralized`/`safe_links_and_data_images_untouched`+実機確認
- [LOW] DNS解決ハング → `resolve_checked`のgetaddrinfoを`--timeout`で囲む。無応答DNSでの無期限ハングを防止（超過は終了コード3）

## プロンプトインジェクション緩和（2026-07-18）→ 対応済み（テスト+文書）

完全防御はツール単体では不可能（消費側の責務）という前提のもと、攻撃面の縮小と来歴明示を実装。

- [A] 本文からのwebgrab制御マーカー偽造防止 → `[webgrab:`を`[quoted-webgrab:`へ無害化（常時ON、大小無視）。テスト`body_cannot_forge_reserved_marker`/`real_footer_marker_still_uses_reserved_prefix`
- [B] `--fence`で本文を`[webgrab:untrusted-content ...]`〜`untrusted-content-end`で囲む。境界に予約マーカー名前空間を使い、本文からは閉じマーカーを偽造不可。テスト`fence_wraps_body_and_body_cannot_close_it`/`fence_html_uses_comments`
- [C] エージェント用途はJSON出力（本文がフィールド分離）を推奨 → README/SKILLに明記
- [D] 信頼モデル（取得内容は非信頼・指示の自動実行禁止・破壊操作は人間確認）→ README/SKILLに明記
- 根本の説得型テキスト注入は消費側（権限分離・自動実行禁止・人間確認）でのみ緩和可、と各文書に明記

## 依存の注記（2026-07-17）

- 直接依存はすべて最新。transitiveのgeneric-array 0.14.7のみ上流のsemver制約で0.14.9より後方（cargo update --verboseで確認）

## 実装時の計画逸脱（2026-08-27）

08-js-render-design.mdおよび09-js-render-implementation-plan.mdの記述と、実装時に確定した挙動との差分を記録する。

- `sanitize_detail`はANSI CSIシーケンス（`ESC [ ... 終端文字`）も除去する。09の計画本文はC0・DEL・C1のみを対象としていたが、Task 6のブリーフが要求したテストがCSIシーケンスの除去も検証していたため、実装をテストに合わせて拡張した
- `visible_text_len`のテスト期待値を39文字・12文字に修正した。計画本文（Task 2）はアンカーテキストの文字数を誤って数えており、実装時に実際のHTML断片を数え直して訂正した。あわせてエンティティのデコード順序を変更し、`&amp;`を最後にデコードするようにした（先にデコードすると二重デコードで文字化けする経路があったため）
- `InFlight`のtombstoneが上限（4096件）に達した場合、実装は最古のものから捨てる（設計08 §4.2 (g)のとおり）。09の計画コードはこの上限処理を単純化し`clear()`していたため、設計の記述に合わせて実装した
- プロキシ層の`exceeded()`は終了コード4へ写像する（設計08 §4.2手順7）。09の計画コードにはこの写像が欠けていたため、実装時に追加した
- E2Eのfixture番兵文字列は`_`ではなく`-`区切りにした（例: `SENTINEL-FAST`）。htmdが`_`をMarkdownのイタリック記法としてエスケープし、番兵文字列がstdoutにそのまま現れなくなるため
- gzip fixture（`tests/fixtures/big_gzip.html.gz`）は展開後2MiBを`gzip -9`した実測約2.1KiBで、計画が見込んでいた約4KiBより小さい
- カバレッジは除外なしで計測し、ローカル実測で91.87%（line coverage）だった。設計08 §3決定表・Global Constraint 5の「除外なしで80%達成」を選択した（除外を戻す代替案は不採用）
- 実装完了後のTask 11bで、当初計画にないレビュー指摘の一括対応（Minor 18件の修正）をユーザー指示によりPR作成前に実施した
