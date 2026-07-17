#!/usr/bin/env python3
"""doclint — docs/ 配下のMarkdown文書の機械検査。

SSOT: このファイルの RULES 表が真、docs/_quality/STYLE_GUIDE.md はその写し。
検出と置換は同一の境界つき正規表現を使うこと（素朴なsubstring一致は禁止）。
使い方: python3 tools/doclint.py [対象ディレクトリ (default: docs)]
出力: [severity] path:line message / 末尾に件数行。Critical+High>0 で exit 1。
"""
import pathlib
import re
import sys

# (severity, compiled regex, message)
RULES = [
    ("Critical", re.compile(r"(?<![A-Za-z])TBD(?![A-Za-z])"), "未決定マーカーTBDが残っている（決定凍結違反）"),
    ("Critical", re.compile(r"後で決める"), "未決定表現が残っている（決定凍結違反）"),
    ("High", re.compile(r"(?<![a-zA-Z])(WebGrab|Webgrab|webGrab|WEBGRAB)(?![a-z])"),
     "表記ゆれ: 正準形は webgrab（小文字）"),
    ("High", re.compile(r"マークダウン"), "表記ゆれ: 正準形は Markdown"),
    ("High", re.compile(r"(?<![a-zA-Z])Javascript(?![a-zA-Z])"), "表記ゆれ: 正準形は JavaScript"),
    ("Medium", re.compile(r"：$"), "文末コロン（japanese-writing-style違反）"),
]


def lint_file(path: pathlib.Path) -> list[tuple[str, str]]:
    findings = []
    in_fence = False
    for i, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
        if line.lstrip().startswith("```"):
            in_fence = not in_fence
            continue
        if in_fence or "`" in line:  # コードブロック・インラインコードを含む行は対象外
            continue
        for sev, rx, msg in RULES:
            if rx.search(line):
                findings.append((sev, f"[{sev}] {path}:{i} {msg}"))
    return findings


def main() -> int:
    root = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else "docs")
    counts = {"Critical": 0, "High": 0, "Medium": 0, "Low": 0}
    for md in sorted(root.rglob("*.md")):
        if "_quality" in md.parts:
            continue
        for sev, msg in lint_file(md):
            counts[sev] += 1
            print(msg)
    print(f"Critical {counts['Critical']} / High {counts['High']} / "
          f"Medium {counts['Medium']} / Low {counts['Low']}")
    return 1 if counts["Critical"] + counts["High"] else 0


if __name__ == "__main__":
    sys.exit(main())
