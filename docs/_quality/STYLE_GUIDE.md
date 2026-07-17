# スタイルガイド（tools/doclint.sh のルール表の人間可読版）

SSOTは `tools/doclint.sh` 内のルール表です。本書はその写しであり、片方だけの変更は禁止です（同一編集で両方を更新すること）。

| Severity | ルール | 正準形/理由 |
|---|---|---|
| Critical | `TBD` の残存禁止 | 決定凍結（MECE決定表にTBD行を残さない） |
| Critical | 「後で決める」の残存禁止 | 同上 |
| High | WebGrab / Webgrab / webGrab / WEBGRAB 禁止 | 正準形は webgrab（小文字） |
| High | 「マークダウン」禁止 | 正準形は Markdown |
| High | Javascript 禁止 | 正準形は JavaScript |
| Medium | 文末コロン（：で終わる行）禁止 | japanese-writing-style 準拠 |

検出・置換とも同一の境界つき正規表現を使うこと。素朴な部分文字列一致（substring in）は検出にも置換にも禁止。
