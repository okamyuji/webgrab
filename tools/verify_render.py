#!/usr/bin/env python3
"""webgrab --render の取得結果を、独立経路（headless Chrome --dump-dom）の可視テキストと突合する。
使い方: python3 tools/verify_render.py /tmp/webgrab-verify
"""
import sys, json, glob, re
from html.parser import HTMLParser


class Extract(HTMLParser):
    def __init__(self):
        super().__init__()
        self.headings, self.paras, self.cur, self.skip = [], [], None, 0

    def handle_starttag(self, tag, attrs):
        if tag in ("script", "style", "noscript"):
            self.skip += 1
        if tag in ("h1", "h2"):
            self.cur = ("h", "")
        elif tag == "p":
            self.cur = ("p", "")

    def handle_endtag(self, tag):
        if tag in ("script", "style", "noscript"):
            self.skip = max(0, self.skip - 1)
        if self.cur and tag in ("h1", "h2", "p"):
            kind, txt = self.cur
            txt = re.sub(r"\s+", " ", txt).strip()
            (self.headings if kind == "h" else self.paras).append(txt)
            self.cur = None

    def handle_data(self, data):
        if self.skip == 0 and self.cur:
            self.cur = (self.cur[0], self.cur[1] + data)


def main(root):
    rows = []
    for dom in sorted(glob.glob(f"{root}/*.dom.html")):
        base = dom[: -len(".dom.html")]
        ex = Extract()
        ex.feed(open(dom, encoding="utf-8", errors="replace").read())
        head = next((x for x in ex.headings if len(x) >= 4), "")
        para = next((x for x in ex.paras if len(x) >= 30), "")
        for mode in ("render", "auto", "rawrender"):
            if mode == "rawrender" and not glob.glob(f"{base}.{mode}.json"):
                continue
            try:
                j = json.load(open(f"{base}.{mode}.json", encoding="utf-8"))
            except Exception as e:  # 出力なし（終了コード非0）
                rows.append((base.rsplit("/", 1)[-1], mode, head[:40], para[:40], "NO-OUTPUT", str(e)[:60]))
                continue
            md = j.get("markdown", "")
            # readabilityはh1をtitleへ移すため、見出しはmarkdownまたはtitleのどちらかに含まれれば一致とする
            title = j.get("title") or ""
            ok_h = head != "" and (head in md or head in title)
            ok_p = para != "" and para[:30] in md
            p_verdict = "比較対象なし" if para == "" else ("一致" if ok_p else "不一致")
            verdict = f"heading={'一致' if ok_h else '不一致'} paragraph={p_verdict}"
            rows.append((base.rsplit("/", 1)[-1], mode, head[:40], para[:40], j.get("render_status"), verdict))
    print("| URL | mode | 見出し | 先頭段落(先頭40字) | render_status | 判定 |")
    print("|---|---|---|---|---|---|")
    for r in rows:
        print("| " + " | ".join(str(x) for x in r) + " |")


if __name__ == "__main__":
    main(sys.argv[1] if len(sys.argv) > 1 else "/tmp/webgrab-verify")
