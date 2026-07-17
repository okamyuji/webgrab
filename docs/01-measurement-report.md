# 計測報告書 — LLM Web検索の取得量制限の実測

- バージョン: 1.0
- 日付: 2026-07-17
- 計測環境: Claude Code (claude-fable-5)、headlessモード（`claude -p --setting-sources "project"`、プラグインフック無効）
- 対応する計画: [00-investigation-plan.md](00-investigation-plan.md)

## 結論（主張の真偽判定）

| 主張 | 判定 | 根拠 |
|---|---|---|
| 「web searchは先頭約2000バイトしか取得しない」 | WebFetchについては偽 / WebSearchについてはほぼ真 | WebFetchの切断点は正確に100,000文字。WebSearchがメインモデルへ渡すのはリンク10件+約1,900文字の生成要約のみ |
| 「要約だけを取得している」 | 構造的に真 | WebFetch・WebSearchとも、メインモデルには原文でなく別モデル/サーバ側が生成した応答テキストのみが渡る |

## 計測B1: WebFetchの取得上限（実測）

### 実験1 — RFC 9110全文テキスト（502,907文字）

- 手順: 事前にcurlで全文を取得し各オフセットのプローブ文字列を確定。headless ClaudeにWebFetchを1回だけ実行させ、可視範囲を回答させた。
- 観測:
  - 可視: オフセット2,000（license-info）、10,026（目次の414 URI Too Long）、20,000付近（Authentication-Info表）
  - 不可視: オフセット200,000（deactivated or archived）、文書末尾（greenbytes GmbH）
  - 受信内容の末尾: `"1.  If the request metho"` — 全文中のオフセット99,980に位置し、99,980+20=**ちょうど100,000文字目**で切断
- Ran: `claude -p --allowedTools WebFetch`（RFC 9110） → Exit: 0 → Observed: 切断点=100,000文字ちょうど

### 実験2 — Project Gutenberg pg2600.txt（3,293,655文字）

- 観測: 受信内容の末尾は `"My pet, whose name day it is. My dear pet!"` — 全文中オフセット約99,998で終了。**再現性あり（2/2で100,000文字切断）**
- Ran: `claude -p --allowedTools WebFetch`（pg2600.txt） → Exit: 0 → Observed: 切断点≈100,000文字

### 実験3 — 対照実験（測定系の妥当性）

- Ran: `curl localhost:8799/marker_page.html | grep -c MARKER` → Exit: 0 → Observed: 200/200マーカー+終端文が配信されることを確認（測定ページ自体は正しい）
- 備考: WebFetchはlocalhost URLを受け付けない（"Invalid URL"）ため、公開URL（RFC/Gutenberg）方式に切り替えた

## 計測A1/A2: WebSearchの返却内容（実測）

- 手順: headless ClaudeにWebSearchを1回実行させ、生ツール結果を全文ファイルにダンプして計測
- 観測:
  - 返却物 = ①リンク10件のJSON（title+URLのみ、本文スニップなし） ②サーバ側生成の要約文 約1,900文字
  - 各ページの原文本文はメインモデルに一切渡らない
  - 生成要約には事実誤認が含まれうることも同時に観測された（RFC 2295の内容をRFC 9110のものとして記述、SHOULDをmustと記述）
- Ran: WebSearchダンプ → Exit: 0 → Observed: ダンプ全体4,128バイト、うち要約プローズ部 約1,900文字

## WebFetchの「要約のみ」性（B2）

WebFetchツールの仕様上、URLの取得内容はmarkdown変換後に小型高速モデルへ渡され、メインモデルが受け取るのは**その小型モデルの応答文のみ**である。今回の実測でも、ツール結果は原文でなく小型モデルの回答文だった。つまり原文がそのままメインモデルの文脈に入ることはない。

## 未確認事項

- Zenn記事の原典URL（検索では特定できず）。判定は記事でなく主張そのものに対して行った
- Claude web UI（claude.ai）やAPIのweb search toolの内部値（本環境から観測不能）
- ChatGPT/Kimi等の他社実装（観測不能）

## 総合判定

主張は「数値（2000バイト）は不正確だが、本質（LLMはWebページの原文全体を受け取っておらず、要約・切断された情報しか見ていない）は真」。したがって、原文を確実に・制御可能な形でLLMの文脈へ渡すRust製Web取得CLIには実用上の存在意義がある。
