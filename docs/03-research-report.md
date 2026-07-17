# Deep Research報告書 — webgrab設計のための先行技術調査

- バージョン: 1.0
- 日付: 2026-07-17
- 手法: 3並列調査エージェント（A: 先行ツール / B: crate選定 / C+D: 出力設計・運用）、WebSearch + crates.io/GitHub APIの実測値に基づく
- 対応する計画: [02-research-plan.md](02-research-plan.md)

## エグゼクティブサマリ

fetch + 本文抽出 + Markdown出力 + JSレンダリング + トークン制御を1つの成熟した単一バイナリで満たすRust製CLIは存在しない（近縁候補はスター一桁〜DL数百の実験段階のみ）。部品となるcrateは揃っており、webgrabの価値は「Jina Reader互換の出力形式」と「トークン制御」を統合する層にある。新規開発の必要性は肯定された（Rule 8ラダーの「作らずに済むか」検証済み）。

## A. 先行ツール調査の要点

| ツール | 分類 | 判定 |
|---|---|---|
| monolith (Rust, ★15k) | ページ丸ごとHTML保存 | 非競合（アーカイブ用途、Markdown化しない） |
| trafilatura (Python, ★6.3k) | 本文抽出CLI | 品質は高いがPython環境前提。記事系に最適化 |
| Jina Reader (★11.6k) | LLM向けMarkdown API | 出力形式のデファクト。`Title:` / `URL Source:` ヘッダ+Markdown本文 |
| Firecrawl (★152k) | スクレイピングAPI/SaaS | Docker前提のサーバで単一バイナリではない |
| url2md / browser39 / acrawl 等 | Rust製の近縁CLI | JSレンダリング非対応または実験段階（DL数十〜数百） |

出典は各調査エージェント報告（GitHub/crates.io数値は2026-07-17時点のAPI実測値）。主要リンク: [monolith](https://github.com/Y2Z/monolith), [trafilatura](https://github.com/adbar/trafilatura), [jina-ai/reader](https://github.com/jina-ai/reader), [firecrawl](https://github.com/firecrawl/firecrawl)

## B. 技術要素の採用決定（MECE決定表）

| 決定 | 選択肢 | 採用 | 理由 |
|---|---|---|---|
| HTTP取得 | reqwest / ureq | reqwest v0.13系 | JSレンダリングでtokioが必要になるため非同期版が自然。デファクトでメンテ最厚 |
| JSレンダリング | chromiumoxide / headless_chrome / chrome --dump-domサブプロセス | chromiumoxide（本報告の「Chrome未検出時は静的取得へフォールバック」案は04-design.mdで不採用に確定。終了コード7方式） | CDP制御が細かく開発が活発。--dump-domは完了待ち制御不能でSPAに不向き |
| 本文抽出 | dom_smoothie / readability / llm_readability / extractous | dom_smoothie v0.18系 | readability.js忠実移植で2026-06更新。第三者ベンチで旧readability系は本文53バイトのみ返す失敗例あり |
| HTML→Markdown | htmd / fast_html2md / html2md | htmd v0.5系 | 品質・DL数（117万/直近）・Apache-2.0。html2mdはGPLかつ出力肥大例あり |
| トークン概算 | tiktoken-rs / 文字数近似 | tiktoken-rs（o200k_base） | 日本語で文字数/4近似は過小見積もりし予算超過を招く。安全側のBPE実測 |
| 文字コード | encoding_rs単体 / +chardetng | encoding_rs + chardetng | reqwestのtext()はmeta charsetを見ないため、ヘッダ→metaスニッフ→chardetng推定の3段判定が定石 |

出典: [dom_smoothie](https://github.com/niklak/dom_smoothie), [htmd](https://github.com/letmutex/htmd), [tiktoken-rs](https://github.com/zurawiki/tiktoken-rs), [chardetng](https://github.com/hsivonen/chardetng), [13 crate比較ベンチ](https://emschwartz.me/comparing-13-rust-crates-for-extracting-text-from-html/), [chromiumoxide vs headless_chrome比較](https://dev.to/vhub_systems_ed5641f65d59/headless-browsers-in-rust-chromiumoxide-vs-headlesschrome-vs-the-python-alternative-25e5)

## C. LLM向け出力設計の要点

- ヘッダ形式のデファクトはJina Reader型（`Title:` / `URL Source:` / `Published Time:`（取得できた場合） / `Markdown Content:`）。プログラム連携はFirecrawl型のJSONエンベロープ（markdown + metadata分離）
- 続き取得の実績ある操作モデルはAnthropic公式MCP fetchサーバの`start_index`/`max_length`（文字ベースページング、デフォルト5,000文字）
- Claude CodeのBashツールは約30,000文字で出力を切り詰めるため、デフォルト出力はその内側に収め、切り詰め時は「続きを取るコマンド」を自己記述的フッタで提示するのが安全
- 本文はstdout、診断はstderr、失敗は必ず非0終了。エージェントは終了コードで成否判定する

出典: [MCP fetchサーバ](https://github.com/modelcontextprotocol/servers/tree/main/src/fetch), [Firecrawl scrape docs](https://docs.firecrawl.dev/features/scrape), [claude-code issue #19901](https://github.com/anthropics/claude-code/issues/19901)

## D. 運用・安全の要点

- robots.txt: MCP fetchサーバはデフォルト尊重+`--ignore-robots-txt`で解除。webgrabもデフォルト尊重+`--no-robots`解除を採用（安全側）
- User-Agent: `webgrab/<version> (+プロジェクトURL)` 形式でボット名を明示し、`--user-agent`で上書き可能に
- 終了コード: sysexitsは非推奨のため、少数の自前コードを`--help`に明文化（本節で例示した暫定割当は04-design.md 5章の終了コード表で置換済み。正は04の表）

出典: [MCP fetchサーバ](https://github.com/modelcontextprotocol/servers/tree/main/src/fetch), [sysexits(3)](https://man.freebsd.org/cgi/man.cgi?query=sysexits&sektion=3)

## 未解決事項（設計へ引き継ぎ）

- chromiumoxide v0.9.1と最新Chrome安定版のCDP互換性は未確認（確信度: 中）。実装フェーズの最初にsmoke testで検証する
- Codex CLI / Kimi CLIの出力サイズ上限の一次ソース値は情報不足。Claude Codeの30,000文字を最も厳しい制約として設計する
