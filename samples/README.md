# samples — 各LLMからwebgrabを呼び出すサンプル

このディレクトリは**サンプル集**です。ここに置かれたSKILLや設定は、webgrabを利用する側のLLMエージェントに向けたものであり、**このリポジトリ自体の開発に適用されるルール（`.claude/`等）とは明確に別物**です。コピーして各自の環境に配置してください。

## 構成

```text
samples/skills/
├── claude/webgrab/SKILL.md   # Claude Code 用スキル（~/.claude/skills/ 等へ）
├── codex/AGENTS.md            # Codex CLI 用の呼び出しガイド断片
└── kimi/webgrab-tool.md       # Kimi CLI 用の呼び出しガイド断片
```

## 共通の使い所

エージェントが「ページ本文を読みたい」「検索結果のURLの中身を確認したい」「SPAの内容を取得したい」ときに、`curl`や組み込みのfetchではなくwebgrabを使うと、本文抽出済み・トークン量制御済み・続き取得可能なMarkdownが得られます。

各ファイルは、そのエージェントの慣習（Claudeはskill、CodexはAGENTS.md、Kimiはツールガイド）に合わせた最小の呼び出し指示です。
