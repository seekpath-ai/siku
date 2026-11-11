#!/usr/bin/env python3
"""render_preview.py — 把 .xlsx 渲染成 PNG，供视觉自检。

依赖：soffice（LibreOffice，xlsx→pdf）+ pypdfium2（pdf→png）。
用法：python3 render_preview.py <file.xlsx> [输出目录]
成功时逐行打印 PNG 绝对路径；缺依赖时退出码 2 并说明跳过原因。
"""

import shutil
import subprocess
import sys
import tempfile
from pathlib import Path


def main() -> None:
    if len(sys.argv) < 2:
        sys.stderr.write("用法：python3 render_preview.py <file.xlsx> [输出目录]\n")
        sys.exit(1)
    src = Path(sys.argv[1]).resolve()
    if not src.is_file():
        sys.stderr.write(f"文件不存在：{src}\n")
        sys.exit(1)

    soffice = shutil.which("soffice") or shutil.which("libreoffice")
    if not soffice:
        sys.stderr.write("未检测到 LibreOffice（soffice），跳过视觉自检\n")
        sys.exit(2)
    try:
        import pypdfium2 as pdfium
    except ImportError:
        sys.stderr.write("缺少依赖：pypdfium2。请通过 skill_env_setup 或插件页安装隔离环境依赖\n")
        sys.exit(2)

    out_dir = Path(sys.argv[2]).resolve() if len(sys.argv) > 2 else src.parent / f"{src.stem}_preview"
    out_dir.mkdir(parents=True, exist_ok=True)

    with tempfile.TemporaryDirectory() as tmp:
        r = subprocess.run(
            [soffice, "--headless", "--convert-to", "pdf", "--outdir", tmp, str(src)],
            capture_output=True, text=True, timeout=180,
        )
        pdf_path = Path(tmp) / f"{src.stem}.pdf"
        if r.returncode != 0 or not pdf_path.is_file():
            sys.stderr.write(f"LibreOffice 转换失败：{r.stderr.strip() or r.stdout.strip()}\n")
            sys.exit(1)

        pdf = pdfium.PdfDocument(str(pdf_path))
        for i, page in enumerate(pdf, 1):
            # 2x 缩放≈144dpi，自检足够清晰且文件不大。
            bitmap = page.render(scale=2.0)
            png = out_dir / f"page-{i:02d}.png"
            bitmap.to_pil().save(str(png))
            print(png)
        pdf.close()


if __name__ == "__main__":
    main()
