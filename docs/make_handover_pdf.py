#!/usr/bin/env python3
"""docs/HANDOVER.md を PDF にする（Windows / Chrome か Edge の印刷エンジンを使う）。

使い方:
    python docs/make_handover_pdf.py
    python docs/make_handover_pdf.py --md docs/HANDOVER.md --pdf docs/HANDOVER.pdf

必要なもの:
    - Python 3.10 以上 + `pip install markdown-it-py`
    - Google Chrome または Microsoft Edge（--headless の --print-to-pdf を使う）

日本語は Windows 標準の游ゴシック / メイリオで組む。CSS はこのファイル内にある。
"""

from __future__ import annotations

import argparse
import html
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

from markdown_it import MarkdownIt

TITLE = "引き継ぎ資料（Antigravity 向け）"
SUBTITLE = "OBS Noise Suppressor / DPDFNet Voice — 開発・配布・GitHub 管理の引き継ぎ"

CSS = """
@page { size: A4; margin: 16mm 15mm 18mm 15mm; }
* { box-sizing: border-box; }
html { -webkit-print-color-adjust: exact; print-color-adjust: exact; }
body {
  font-family: "Yu Gothic UI", "Yu Gothic", "Meiryo", "MS Gothic", sans-serif;
  font-size: 9.6pt; line-height: 1.62; color: #1b1b1b; margin: 0;
}
h1, h2, h3, h4 { font-family: "Yu Gothic", "Meiryo", sans-serif; line-height: 1.35;
  break-after: avoid; page-break-after: avoid; }
h1 { font-size: 17pt; margin: 0 0 2mm; padding-bottom: 2.5mm;
  border-bottom: 2.2px solid #2c5d8f; color: #12395c; }
h2 { font-size: 12.4pt; margin: 7mm 0 2.2mm; padding: 1.2mm 0 1.2mm 2.6mm;
  border-left: 4px solid #2c5d8f; background: #eef3f8; color: #12395c; }
h3 { font-size: 10.6pt; margin: 5mm 0 1.6mm; color: #1d4c76; }
h4 { font-size: 9.8pt; margin: 3.5mm 0 1.2mm; color: #333; }
p { margin: 0 0 2.2mm; }
ul, ol { margin: 0 0 2.4mm; padding-left: 6mm; }
li { margin: 0 0 0.9mm; }
li > ul, li > ol { margin-top: 0.9mm; margin-bottom: 0; }
li > p { margin: 0 0 1.2mm; }
a { color: #1a5c9c; text-decoration: none; word-break: break-all; }
hr { border: 0; border-top: 1px solid #cfd8e0; margin: 6mm 0; }
strong { color: #0d2b45; }
code {
  font-family: Consolas, "Cascadia Mono", "MS Gothic", monospace;
  font-size: 8.4pt; background: #f1f3f5; border: 1px solid #e2e6ea;
  border-radius: 2px; padding: 0 1px;
}
pre {
  background: #f6f8fa; border: 1px solid #dde3e9; border-left: 3px solid #9fb6cc;
  border-radius: 3px; padding: 2.2mm 3mm; margin: 0 0 3mm; overflow: visible;
  white-space: pre-wrap; word-break: break-all;
  break-inside: avoid; page-break-inside: avoid;
}
pre code { background: none; border: 0; padding: 0; font-size: 8.1pt; line-height: 1.45; }
blockquote {
  margin: 0 0 3mm; padding: 1.6mm 3mm; background: #fdf7e6;
  border-left: 3px solid #d8b45c; color: #4a3c1c;
}
blockquote p { margin: 0 0 1mm; }
table { border-collapse: collapse; width: 100%; margin: 0 0 3.4mm; font-size: 8.8pt; }
thead { display: table-header-group; }
tr { break-inside: avoid; page-break-inside: avoid; }
th, td { border: 1px solid #c9d3dc; padding: 1.1mm 1.8mm; text-align: left;
  vertical-align: top; }
th { background: #e8eef4; color: #12395c; font-weight: 600; }
tbody tr:nth-child(even) td { background: #fafbfc; }
td code, th code { font-size: 8.1pt; }
.meta { color: #5a6672; font-size: 8.6pt; margin: 0 0 5mm; }
"""

# 見つかった順に使う（Chrome 優先。どちらも --headless=new --print-to-pdf に対応）
BROWSERS = [
    r"C:\Program Files\Google\Chrome\Application\chrome.exe",
    r"C:\Program Files (x86)\Google\Chrome\Application\chrome.exe",
    r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe",
    r"C:\Program Files\Microsoft\Edge\Application\msedge.exe",
]


def find_browser() -> str:
    for path in BROWSERS:
        if Path(path).is_file():
            return path
    for name in ("chrome", "msedge"):
        found = shutil.which(name)
        if found:
            return found
    raise SystemExit("Chrome / Edge が見つかりません（--print-to-pdf に必要）")


def render_html(md_text: str) -> str:
    md = MarkdownIt("gfm-like", {"html": True, "linkify": False, "typographer": False})
    body = md.render(md_text)
    # 先頭の h1（資料タイトル）はヘッダ側で描くので落とす
    body = re.sub(r"^\s*<h1>.*?</h1>\s*", "", body, count=1, flags=re.S)
    # 「64,569,098 B」の数値と単位の間で改行させない
    body = re.sub(r"(\d[\d,]*)\s+B(?![A-Za-z0-9])", r"\1&nbsp;B", body)
    return f"""<!DOCTYPE html>
<html lang="ja">
<head>
<meta charset="utf-8">
<title>{html.escape(TITLE)}</title>
<style>{CSS}</style>
</head>
<body>
<h1>{html.escape(TITLE)}</h1>
<div class="meta">{html.escape(SUBTITLE)}</div>
{body}
</body>
</html>
"""


def main() -> int:
    root = Path(__file__).resolve().parent.parent
    ap = argparse.ArgumentParser(description="HANDOVER.md を PDF にする")
    ap.add_argument("--md", default=str(root / "docs" / "HANDOVER.md"))
    ap.add_argument("--pdf", default=str(root / "docs" / "HANDOVER.pdf"))
    args = ap.parse_args()

    md_path = Path(args.md).resolve()
    pdf_path = Path(args.pdf).resolve()
    if not md_path.is_file():
        raise SystemExit(f"Markdown が見つかりません: {md_path}")

    browser = find_browser()
    with tempfile.TemporaryDirectory() as tmp:
        html_path = Path(tmp) / "handover.html"
        html_path.write_text(render_html(md_path.read_text(encoding="utf-8")), encoding="utf-8")
        pdf_path.parent.mkdir(parents=True, exist_ok=True)
        if pdf_path.exists():
            pdf_path.unlink()
        url = html_path.as_uri()
        proc = subprocess.run(
            [
                browser,
                "--headless=new",
                "--disable-gpu",
                "--no-pdf-header-footer",
                "--run-all-compositor-stages-before-draw",
                "--virtual-time-budget=8000",
                f"--print-to-pdf={pdf_path}",
                url,
            ],
            capture_output=True,
            text=True,
        )
        if proc.returncode != 0 or not pdf_path.is_file():
            print(proc.stdout[-2000:], file=sys.stderr)
            print(proc.stderr[-2000:], file=sys.stderr)
            raise SystemExit(f"PDF の生成に失敗しました (exit {proc.returncode})")

    print(f"PDF: {pdf_path} ({pdf_path.stat().st_size:,} B)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
