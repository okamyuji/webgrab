# 実動作検証計画書 — webgrab

- バージョン: 1.0
- 日付: 2026-07-17
- 対象設計: [04-design.md](04-design.md) v1.2 §9 完了条件

## MECEロジックツリー（何を検証すれば「実動作する」と言えるか）

```text
webgrabの実動作検証
├── A. 静的検証（コード品質）
│   ├── A1. cargo build --release がexit 0
│   ├── A2. cargo test（unit+integration）がexit 0
│   ├── A3. cargo clippy -D warnings がexit 0
│   └── A4. カバレッジ 60%超（目標80%）
├── B. 実URL取得（設計§9-5の3種）
│   ├── B1. 大型ページ（10万字超）: WebFetchの100,000字切断を超える取得+ページング
│   ├── B2. 日本語ページ: 文字コード判定・title/公開日時抽出・マルチバイト境界スライス
│   └── B3. SPA/JSレンダリング: --renderのCDP Fetch interception経路が通ること
├── C. 続き取得・形式
│   ├── C1. --start-indexで続きが正しいオフセットから取れる
│   └── C2. --format jsonが妥当なJSONを出す（統合テストで担保）
└── D. エラー・安全経路（統合テストで担保）
    ├── D1. HTTP 4xx → exit 4
    ├── D2. robots拒否 → exit 5
    ├── D3. 内部アドレス → exit 8（SSRF防止）
    └── D4. 不正スキーム → exit 2
```

## 完了条件

上記A〜Dすべてに観測エビデンス（コマンド・終了コード・出力の一部）が [07-verification-report.md](07-verification-report.md) に記録されていること。

## やらないこと

- 全世界のサイトでの網羅テスト（代表3種で代替）
- 負荷試験・ベンチマーク（スコープ外）
