# 改善バックログ

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
- robots取得のnetguard通過・サイズ上限・手動リダイレクト（C=Medium, A=Medium 合意）: robots.rs仕様に反映
- 継続コマンドの--start-index衝突（B=High, A=Low、契約と実装の不一致）: §6で置換規則を明記
- --max-chars=0/終端フッタの形式横断（B=High/Medium, A=Low）: §6で全形式定義
- -o継続コマンドの上書き（B=Low, A=Low 合意）: §6で-o除外
- robots UA製品トークン照合規則（A=Medium 単独、契約完全性）: robots.rs仕様に反映
- IPv4-mapped IPv6正規化・`::1`/`::`拒否（C=Medium 単独、セキュリティ）: §3.1に反映
- stderr error=トークン一覧（A=Low 単独）: 終了コード表にerror=列追加

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
- M4 同一ホップでのDNS二重解決→ 解決済みaddrをrobots_allowedへ渡して回避
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
