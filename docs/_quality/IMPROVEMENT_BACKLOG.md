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

## 依存の注記（2026-07-17）

- 直接依存はすべて最新。transitiveのgeneric-array 0.14.7のみ上流のsemver制約で0.14.9より後方（cargo update --verboseで確認）
