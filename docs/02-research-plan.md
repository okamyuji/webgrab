# Deep Research計画書 — webgrab設計のための先行技術調査

- バージョン: 1.0
- 日付: 2026-07-17
- 確定要件（ユーザー回答）: fetch専用 / JSレンダリング必要 / デフォルトMarkdown出力 / ツール名 webgrab

## MECEロジックツリー（何を調べれば設計が決まるか）

```text
webgrab設計に必要な知識
├── A. 先行ツール（何が既にあり、何が足りないか）
│   ├── A1. Rust製: monolith, readability系crate, htmd/html2md, dom_smoothie
│   ├── A2. 他言語のデファクト: trafilatura(Py), readability(JS), Jina Reader, Firecrawl
│   └── A3. LLMエージェント向け既存CLI（lynx -dump, pandoc, rdrview等）の限界
├── B. 技術要素（どのcrateで何を実現するか）
│   ├── B1. HTTP取得: reqwest（TLS, リダイレクト, 圧縮, タイムアウト）
│   ├── B2. JSレンダリング: chromiumoxide vs headless_chrome vs 外部chrome --dump-dom
│   ├── B3. 本文抽出(boilerplate除去): readability алгоритм系crateの品質比較
│   ├── B4. HTML→Markdown変換: htmd vs html2md vs 自前
│   └── B5. トークン数制御: 文字数上限・分割・tiktoken系crate
├── C. LLM向け出力設計（何をどう出すのがエージェントに最適か）
│   ├── C1. メタデータ（title, url, 取得時刻, 概算トークン数）の付け方
│   ├── C2. 上限超過時の挙動（切断+続き取得 --offset / ページング）
│   └── C3. Claude/Codex/Kimi各ターミナルからの呼び出し形態（Bash経由CLI）
└── D. 運用・安全（壊れない・迷惑をかけない）
    ├── D1. robots.txt/レート制限の扱い
    ├── D2. 文字コード（Shift_JIS等日本語ページ）対応
    └── D3. エラー時の終了コードとメッセージ設計
```

## 調査方法

- WebSearch + 必要に応じてheadless WebFetch / crates.io・GitHub検索
- 各リーフに対し「採用候補・根拠・棄却理由」を`03-research-report.md`に記録する

## 完了条件

- B1〜B5すべてに採用crate（またはアプローチ）が1つ決まり、根拠が記録されている
- A項で「既存ツールをそのまま使えば済む」可能性が棄却または採用されている（Rule 8ラダー: 作らずに済むなら作らない、をまず検証）
