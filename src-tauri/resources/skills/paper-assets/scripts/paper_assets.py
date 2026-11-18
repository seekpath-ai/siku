#!/usr/bin/env python3
"""paper_assets.py — 把图表插入 Word / PowerPoint，并生成图注和编号。

用法:
    python3 paper_assets.py --schema
    python3 paper_assets.py insert --target paper.docx --image figure.png \
        --caption "Overall architecture." --output paper-with-figure.docx

依赖: python-docx / python-pptx（装在工作区虚拟环境）。
"""

from __future__ import annotations

import argparse
import json
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path
from typing import Any

SCHEMA: dict[str, Any] = {
    "command": "insert",
    "target": "输入 .docx / .pptx（不会被修改）",
    "output": "输出 .docx / .pptx（不能与 target 相同）",
    "image": "图片路径；支持 png/jpg/jpeg/webp，或可转换的 pdf/svg",
    "caption": "图注文字（不含编号）",
    "label": "可选：Figure | 图，默认 Figure",
    "number": "可选：强制编号；省略则扫描文档取最大编号 +1",
    "width_in": "Word 图片宽度（英寸），默认 6.0",
    "after": "Word 可选：插入到包含该文字的段落之后；省略则追加到末尾",
    "slide_index": "PPT 可选：插入已有页（0 开始）；省略则新建一页",
}


def fail(message: str) -> None:
    sys.stderr.write(message.rstrip() + "\n")
    raise SystemExit(1)


def load_docx():
    try:
        from docx import Document
        from docx.enum.text import WD_ALIGN_PARAGRAPH
        from docx.shared import Inches, Pt
        return Document, WD_ALIGN_PARAGRAPH, Inches, Pt
    except ImportError as exc:
        fail(
            "缺少 python-docx。请在工作区安装：\n"
            "  uv venv .siku/venv\n"
            "  uv pip install --python .siku/venv/bin/python python-docx\n"
            f"原始错误：{exc}"
        )


def load_pptx():
    try:
        from pptx import Presentation
        from pptx.enum.text import PP_ALIGN
        from pptx.util import Inches, Pt
        return Presentation, PP_ALIGN, Inches, Pt
    except ImportError as exc:
        fail(
            "缺少 python-pptx。请在工作区安装：\n"
            "  uv venv .siku/venv\n"
            "  uv pip install --python .siku/venv/bin/python python-pptx\n"
            f"原始错误：{exc}"
        )


def caption_regex(label: str) -> re.Pattern[str]:
    if label.lower() in {"figure", "fig", "fig."}:
        return re.compile(r"^\s*(?:Figure|Fig\.?)\s*(\d+)\s*[:：.]", re.IGNORECASE)
    if label.strip() == "图":
        return re.compile(r"^\s*图\s*(\d+)\s*[:：.]")
    return re.compile(rf"^\s*{re.escape(label)}\s*(\d+)\s*[:：.]", re.IGNORECASE)


def next_number(texts: list[str], label: str) -> int:
    pattern = caption_regex(label)
    highest = 0
    for text in texts:
        match = pattern.match(text or "")
        if match:
            highest = max(highest, int(match.group(1)))
    return highest + 1


def caption_text(label: str, number: int, caption: str) -> str:
    if label.strip() == "图":
        return f"图 {number}：{caption.strip()}"
    if label.lower() in {"figure", "fig", "fig."}:
        return f"Figure {number}: {caption.strip()}"
    return f"{label} {number}: {caption.strip()}"


def prepare_image(image: Path, workdir: Path) -> Path:
    if not image.is_file():
        fail(f"图片不存在：{image}")
    suffix = image.suffix.lower()
    if suffix in {".png", ".jpg", ".jpeg", ".bmp", ".gif", ".tif", ".tiff"}:
        return image
    target = workdir / "image.png"
    if suffix == ".pdf":
        if shutil.which("pdftoppm"):
            result = subprocess.run(
                ["pdftoppm", "-png", "-r", "300", "-singlefile", str(image), str(target.with_suffix(""))],
                stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, timeout=120,
            )
            if result.returncode == 0 and target.is_file():
                return target
        if shutil.which("convert"):
            result = subprocess.run(
                ["convert", "-density", "300", str(image), str(target)],
                stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, timeout=120,
            )
            if result.returncode == 0 and target.is_file():
                return target
        fail("无法把 PDF 转成 PNG：需要 pdftoppm 或 ImageMagick convert")
    if suffix == ".svg":
        if shutil.which("convert"):
            result = subprocess.run(
                ["convert", "-density", "300", str(image), str(target)],
                stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, timeout=120,
            )
            if result.returncode == 0 and target.is_file():
                return target
        fail("无法把 SVG 转成 PNG：需要 ImageMagick convert 或先在 Inkscape 中导出 PNG")
    fail(f"不支持的图片类型：{suffix}")


def word_texts(doc: Any) -> list[str]:
    out = [p.text for p in doc.paragraphs]
    for table in doc.tables:
        for row in table.rows:
            for cell in row.cells:
                out.extend(p.text for p in cell.paragraphs)
    return out


def insert_docx(args: argparse.Namespace, png: Path) -> int:
    Document, WD_ALIGN_PARAGRAPH, Inches, Pt = load_docx()
    doc = Document(str(args.target))
    number = args.number if args.number is not None else next_number(word_texts(doc), args.label)
    if number < 1:
        fail("编号必须大于 0")
    anchor = None
    if args.after:
        for p in doc.paragraphs:
            if args.after in p.text:
                anchor = p
                break
        if anchor is None:
            fail(f"找不到锚点段落：{args.after}")
    doc.add_picture(str(png), width=Inches(float(args.width_in)))
    pic_paragraph = doc.paragraphs[-1]
    pic_paragraph.alignment = WD_ALIGN_PARAGRAPH.CENTER
    cap_paragraph = doc.add_paragraph()
    cap_paragraph.alignment = WD_ALIGN_PARAGRAPH.CENTER
    try:
        cap_paragraph.style = doc.styles["Caption"]
    except KeyError:
        cap_paragraph.paragraph_format.space_after = Pt(6)
    run = cap_paragraph.add_run(caption_text(args.label, number, args.caption))
    run.italic = True
    run.font.size = Pt(9)
    if anchor is not None:
        anchor._p.addnext(pic_paragraph._p)
        pic_paragraph._p.addnext(cap_paragraph._p)
    doc.save(str(args.output))
    return number


def pptx_texts(prs: Any) -> list[str]:
    out: list[str] = []
    for slide in prs.slides:
        for shape in slide.shapes:
            if getattr(shape, "has_text_frame", False):
                out.append(shape.text)
    return out


def insert_pptx(args: argparse.Namespace, png: Path) -> int:
    Presentation, PP_ALIGN, Inches, Pt = load_pptx()
    try:
        from PIL import Image
        with Image.open(png) as image:
            ratio = image.width / max(1, image.height)
    except Exception:
        ratio = 16 / 9
    prs = Presentation(str(args.target))
    number = args.number if args.number is not None else next_number(pptx_texts(prs), args.label)
    if number < 1:
        fail("编号必须大于 0")
    if args.slide_index is not None:
        if args.slide_index < 0 or args.slide_index >= len(prs.slides):
            fail(f"slide_index 超出范围：{args.slide_index}")
        slide = prs.slides[args.slide_index]
    else:
        layout = prs.slide_layouts[6] if len(prs.slide_layouts) > 6 else prs.slide_layouts[-1]
        slide = prs.slides.add_slide(layout)

    max_w = prs.slide_width - Inches(1.0)
    max_h = prs.slide_height - Inches(1.9)
    width = max_w
    height = int(width / max(ratio, 0.01))
    if height > max_h:
        height = max_h
        width = int(height * ratio)
    left = int((prs.slide_width - width) / 2)
    top = Inches(0.5)
    slide.shapes.add_picture(str(png), left, top, width=width, height=height)
    cap_top = top + height + Inches(0.08)
    if cap_top + Inches(0.5) > prs.slide_height:
        cap_top = prs.slide_height - Inches(0.55)
    box = slide.shapes.add_textbox(left, cap_top, width, Inches(0.45))
    frame = box.text_frame
    frame.clear()
    paragraph = frame.paragraphs[0]
    paragraph.alignment = PP_ALIGN.CENTER
    run = paragraph.add_run()
    run.text = caption_text(args.label, number, args.caption)
    run.font.size = Pt(10)
    run.font.italic = True
    prs.save(str(args.output))
    return number


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description="把图表插入 Word / PowerPoint")
    parser.add_argument("--schema", action="store_true", help="打印 JSON 契约")
    sub = parser.add_subparsers(dest="command")
    ins = sub.add_parser("insert", help="插入一张图片")
    ins.add_argument("--target", required=True)
    ins.add_argument("--output", required=True)
    ins.add_argument("--image", required=True)
    ins.add_argument("--caption", required=True)
    ins.add_argument("--label", default="Figure")
    ins.add_argument("--number", type=int)
    ins.add_argument("--width-in", type=float, default=6.0)
    ins.add_argument("--after")
    ins.add_argument("--slide-index", type=int)
    args = parser.parse_args(argv)
    if args.schema:
        print(json.dumps(SCHEMA, ensure_ascii=False, indent=2))
        return 0
    if args.command != "insert":
        parser.error("需要 insert 子命令，或使用 --schema")
    target = Path(args.target)
    output = Path(args.output)
    image = Path(args.image)
    if not target.is_file():
        fail(f"目标文件不存在：{target}")
    if output.resolve() == target.resolve():
        fail("输出不能覆盖输入文件；请使用新文件名")
    if output.suffix.lower() not in {".docx", ".pptx"}:
        fail("输出必须是 .docx 或 .pptx")
    if target.suffix.lower() != output.suffix.lower():
        fail("输入和输出类型必须一致")
    if not args.caption.strip():
        fail("caption 不能为空")
    if not args.label.strip():
        fail("label 不能为空")
    if not (0.5 <= float(args.width_in) <= 20):
        fail("width_in 必须在 0.5–20 英寸内")
    output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="paper-assets-") as tmp:
        png = prepare_image(image, Path(tmp))
        number = insert_pptx(args, png) if target.suffix.lower() == ".pptx" else insert_docx(args, png)
    note = output.with_name(output.stem + "-INSERT.md")
    note.write_text(
        "# Figure insertion\n\n"
        f"- target: `{target}`\n"
        f"- output: `{output}`\n"
        f"- image: `{image}`\n"
        f"- number: {number}\n"
        f"- caption: {caption_text(args.label, number, args.caption)}\n",
        encoding="utf-8",
    )
    print(json.dumps({
        "output": str(output),
        "number": number,
        "caption": caption_text(args.label, number, args.caption),
        "note": str(note),
    }, ensure_ascii=False))
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
