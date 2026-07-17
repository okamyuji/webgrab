# 実装計画書 — webgrab

- バージョン: 1.0
- 日付: 2026-07-17
- 対象設計: [04-design.md](04-design.md) v1.2（seal済み）

## MECEロジックツリー（実装単位、リスク順）

```text
webgrab実装
├── 1. 基盤（他が依存）
│   ├── 1.1 error.rs（WebgrabError + ExitCode 0-8 + error=トークン）
│   └── 1.2 cli.rs（clap定義、全フラグ）
├── 2. 純ロジック（外部I/O不要、テスト容易・高リスク）
│   ├── 2.1 netguard.rs（IP判定・IPv4-mapped正規化・レンジ照合）★SSRF核心
│   ├── 2.2 robots.rs（パース・ワイルドカード・UA照合・パス一致）
│   ├── 2.3 decode.rs（charset 3段判定）
│   ├── 2.4 budget.rs（char単位スライス・start/max・フッタ生成）
│   └── 2.5 output.rs（5形式の整形）
├── 3. I/O結合（crate結合、実HTTP）
│   ├── 3.1 extract.rs（dom_smoothie）
│   ├── 3.2 convert.rs（htmd）
│   ├── 3.3 fetch.rs（reqwest + IPピン留め + 手動リダイレクト + robots/netguard結合）
│   └── 3.4 render.rs（chromiumoxide、既定計測外）
└── 4. 統合
    ├── 4.1 main.rs（パイプライン結線、ExitCode変換）
    └── 4.2 tests/（TcpListener統合テスト）
```

## 実装順序と各単位の完了条件（機械検証）

リスクの高い純ロジック（netguard, budget, robots）を先に、TDD的に単体テストとともに実装する。各単位はその単体テストがexit 0で完了とする。

1. error.rs → `cargo build`通過
2. netguard.rs + 単体テスト（127.0.0.1 / 10.0.0.1 / 169.254.169.254 / ::1 / ::ffff:10.0.0.1 / 8.8.8.8 の判定、IPv4-mapped正規化）
3. budget.rs + 単体テスト（start=0/中間/末尾超過、max=0/巨大、日本語マルチバイト境界、フッタ・終端フッタ文字列）
4. robots.rs + 単体テスト（Disallow前置、`*`/`$`、UA別グループ、`*`フォールバック、解釈不能→disallow）
5. decode.rs + 単体テスト（UTF-8 / Shift_JIS / EUC-JP / charset無し）
6. output.rs + 単体テスト（5形式のヘッダ・truncated・ended・meta-only）
7. extract.rs / convert.rs（smoke.rsで実挙動確認済みなので薄い結合テスト）
8. fetch.rs（IPピン留め: ClientBuilder::resolve、redirect none手動追従）
9. render.rs（#[ignore] smoke、CDP Fetch interception）
10. cli.rs + main.rs（結線）
11. tests/統合（TcpListenerでローカルHTTP、CLIバイナリ起動、終了コード・stdout/stderr分離）

## テスト計画（カバレッジ60%超・80%目標）

- 純ロジック（netguard/budget/robots/decode/output）は単体テストで高カバレッジを確保。これらがコード量の主体でありカバレッジの土台
- fetch.rsはTcpListener統合テストで主要パス（200正常・404→exit4・リダイレクト・robots拒否→exit5・内部アドレス→exit8）を通す
- render.rsは`--ignore-filename-regex 'render\.rs'`で計測除外し、#[ignore] smokeで担保

## やらないこと

設計§10のスコープ外項目。加えて、実装段階での新機能追加は行わず、設計にない挙動が必要になった場合は設計へ差し戻す。
