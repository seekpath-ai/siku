#!/usr/bin/env python3
"""build_deck.py — 从 JSON 大纲生成符合设计规范的 .pptx。

用法:
    python3 build_deck.py deck.json
    python3 build_deck.py --schema     # 打印 JSON 契约并退出

设计规范（字号/配色/版式）固化在本脚本中，JSON 无法覆盖——这是质量保障
机制的一部分。校验失败时退出码为 1，stderr 说明如何修正。
"""

import json
import sys
from pathlib import Path

try:
    from pptx import Presentation
    from pptx.util import Inches, Pt, Emu
    from pptx.dml.color import RGBColor
    from pptx.enum.text import PP_ALIGN, MSO_ANCHOR
    from pptx.chart.data import CategoryChartData
    from pptx.enum.chart import XL_CHART_TYPE, XL_LEGEND_POSITION
    from pptx.oxml.ns import qn
except ImportError:
    sys.stderr.write("缺少依赖：python-pptx。请运行 pip install --user python-pptx\n")
    sys.exit(2)

# ── 设计令牌（与 references/design-tokens.md 一致，修改需同步） ──

TOKENS = {
    "presentation": {"body": 24, "title": 38, "caption": 20},
    "hall": {"body": 30, "title": 42, "caption": 24},
    "online": {"body": 22, "title": 34, "caption": 18},
    "reading": {"body": 18, "title": 30, "caption": 14},
}

THEMES = {
    "academic": {
        "bg": "FFFFFF", "text": "1F2937", "accent": "1E40AF", "muted": "6B7280",
        "cover_band": False, "bullet_char": "▪",
    },
    "business": {
        "bg": "FFFFFF", "text": "16233A", "accent": "0F4C81", "muted": "5B6472",
        "accent2": "B08D3E", "cover_band": True, "bullet_char": "▪",
    },
    "discussion": {
        "bg": "FDFBF7", "text": "3B322C", "accent": "D97706", "muted": "8A7F74",
        "accent2": "B45309", "cover_band": False, "bullet_char": "•",
    },
}

FONT_LATIN = "Segoe UI"
FONT_CJK_CANDIDATES = ["Microsoft YaHei", "Source Han Sans SC", "Noto Sans CJK SC", "SimHei"]
FONT_CJK = FONT_CJK_CANDIDATES[0]  # 渲染端缺字体时由查看器 fallback，名称仍写入

MAX_BULLETS = 6
WARN_BULLET_CHARS = 45
SLIDE_W = Inches(13.333)
SLIDE_H = Inches(7.5)
MARGIN = Inches(0.7)

SCHEMA = {
    "scenario": "presentation | hall | online | reading（必选）",
    "theme": "academic | business | discussion（必选）",
    "title": "演示总标题（可选）",
    "output": "输出 .pptx 绝对路径（必选）",
    "slides": [
        {"layout": "title", "title": "...", "subtitle": "...", "meta": "汇报人/日期"},
        {"layout": "section", "title": "...", "subtitle": "（可选）"},
        {"layout": "bullets", "title": "...", "bullets": ["..."], "notes": "（可选，演讲者备注）"},
        {"layout": "two_column", "title": "...",
         "left": {"heading": "...", "bullets": ["..."]},
         "right": {"heading": "...", "bullets": ["..."]}},
        {"layout": "text", "title": "...", "paragraphs": ["..."], "notes": "（可选）"},
        {"layout": "image", "title": "...", "image": "绝对路径", "caption": "（可选）"},
        {"layout": "table", "title": "...", "headers": ["列1", "列2"],
         "rows": [["...", "..."]], "caption": "（可选，数据来源）"},
        {"layout": "chart", "title": "...", "chart_type": "bar | line | pie",
         "categories": ["类目A", "类目B"],
         "series": [{"name": "系列名", "values": [1, 2]}],
         "caption": "（可选，单位与来源）"},
        {"layout": "big_number", "title": "...", "number": "42%", "label": "...", "notes": "（可选）"},
        {"layout": "closing", "title": "...", "subtitle": "（可选）"},
    ],
}

TABLE_MAX_COLS = 6
TABLE_MAX_ROWS = 10
CHART_MAX_CATEGORIES = 12
CHART_MAX_SERIES = 4

FORBIDDEN_KEYS = {"font", "font_size", "fontsize", "size", "color", "colour", "bg", "background"}


def fail(msg: str) -> None:
    sys.stderr.write(f"[build_deck] {msg}\n")
    sys.exit(1)


def set_run_font(run, size_pt: int, color_hex: str, bold: bool = False) -> None:
    """同时设置西文与东亚字体（python-pptx 默认只写 latin）。"""
    run.font.size = Pt(size_pt)
    run.font.bold = bold
    run.font.name = FONT_LATIN
    run.font.color.rgb = RGBColor.from_string(color_hex)
    rPr = run._r.get_or_add_rPr()
    for tag in ("a:ea", "a:cs"):
        el = rPr.find(qn(tag))
        if el is None:
            el = rPr.makeelement(qn(tag), {})
            rPr.append(el)
        el.set("typeface", FONT_CJK)


def add_textbox(slide, left, top, width, height):
    box = slide.shapes.add_textbox(left, top, width, height)
    box.text_frame.word_wrap = True
    return box


def add_para(tf, text, size, color, bold=False, align=PP_ALIGN.LEFT,
             space_before=6, first=False, line_spacing=1.3):
    p = tf.paragraphs[0] if first else tf.add_paragraph()
    p.alignment = align
    p.space_before = Pt(space_before)
    p.line_spacing = line_spacing
    run = p.add_run()
    run.text = text
    set_run_font(run, size, color, bold)
    return p


def set_bg(slide, color_hex: str) -> None:
    slide.background.fill.solid()
    slide.background.fill.fore_color.rgb = RGBColor.from_string(color_hex)


def check_no_style_overrides(slide: dict, idx: int) -> None:
    bad = FORBIDDEN_KEYS & {k.lower() for k in slide}
    if bad:
        fail(f"第 {idx} 页包含自定义样式字段 {sorted(bad)}：排版由主题/场景统一控制，请移除这些字段")


def new_slide(prs, theme):
    slide = prs.slides.add_slide(prs.slide_layouts[6])  # blank
    set_bg(slide, theme["bg"])
    return slide


def header(slide, theme, tokens, text):
    """内容页页眉：标题 + 强调色细线。"""
    box = add_textbox(slide, MARGIN, Inches(0.45), SLIDE_W - MARGIN * 2, Inches(1.0))
    add_para(box.text_frame, text, tokens["title"], theme["accent"], bold=True, first=True, space_before=0)
    line = slide.shapes.add_shape(
        1, MARGIN, Inches(1.45), SLIDE_W - MARGIN * 2, Emu(19050))  # 1 = rectangle
    line.fill.solid()
    line.fill.fore_color.rgb = RGBColor.from_string(theme["accent"])
    line.line.fill.background()
    line.shadow.inherit = False


def footer(slide, theme, tokens, page_no):
    box = add_textbox(slide, SLIDE_W - Inches(1.2), SLIDE_H - Inches(0.5), Inches(0.7), Inches(0.35))
    add_para(box.text_frame, str(page_no), tokens["caption"], theme["muted"],
             align=PP_ALIGN.RIGHT, first=True, space_before=0)


def split_bullets(bullets, idx, warnings):
    """超过 MAX_BULLETS 时拆成多页（续页）。单条过长仅警告。"""
    for b in bullets:
        if len(b) > WARN_BULLET_CHARS:
            warnings.append(f"第 {idx} 页要点超长（{len(b)} 字 > {WARN_BULLET_CHARS}）：{b[:20]}… 建议精简")
    return [bullets[i:i + MAX_BULLETS] for i in range(0, len(bullets), MAX_BULLETS)] or [[]]


def add_bullet_slide(prs, theme, tokens, spec, idx, cont_title=None):
    slide = new_slide(prs, theme)
    header(slide, theme, tokens, cont_title or spec["title"])
    box = add_textbox(slide, MARGIN, Inches(1.75), SLIDE_W - MARGIN * 2, SLIDE_H - Inches(2.5))
    tf = box.text_frame
    for i, b in enumerate(spec["bullets"]):
        add_para(tf, f"{theme['bullet_char']}  {b}", tokens["body"], theme["text"],
                 first=(i == 0), space_before=10)
    if spec.get("notes"):
        slide.notes_slide.notes_text_frame.text = spec["notes"]
    return slide


def blend(hex_color: str, other: str, pct: float) -> str:
    """pct=0 → hex_color，1 → other。用于斑马纹与系列色派生。"""
    a = [int(hex_color[i:i + 2], 16) for i in (0, 2, 4)]
    b = [int(other[i:i + 2], 16) for i in (0, 2, 4)]
    return "".join(f"{round(x + (y - x) * pct):02X}" for x, y in zip(a, b))


def add_table_slide(prs, theme, tokens, s, i):
    headers, rows = s.get("headers"), s.get("rows")
    if not s.get("title") or not isinstance(headers, list) or not headers:
        fail(f"第 {i} 页 table 需要 title 和非空 headers 数组")
    if len(headers) > TABLE_MAX_COLS:
        fail(f"第 {i} 页表格列数 {len(headers)} 超过 {TABLE_MAX_COLS} 列，请精简或改用 bullets")
    if not isinstance(rows, list) or not rows:
        fail(f"第 {i} 页 table 需要非空 rows 数组")
    if len(rows) > TABLE_MAX_ROWS:
        fail(f"第 {i} 页表格行数 {len(rows)} 超过 {TABLE_MAX_ROWS} 行，请拆页")
    for r in rows:
        if not isinstance(r, list) or len(r) != len(headers):
            fail(f"第 {i} 页表格行与 headers 列数不一致：{r!r}")
    slide = new_slide(prs, theme)
    header(slide, theme, tokens, s["title"])
    area_w = SLIDE_W - MARGIN * 2
    gf = slide.shapes.add_table(len(rows) + 1, len(headers), MARGIN, Inches(1.75),
                                area_w, SLIDE_H - Inches(2.6))
    tbl = gf.table
    size = max(tokens["caption"], tokens["body"] - 4)
    zebra = blend(theme["bg"], "000000", 0.05)
    for c, h in enumerate(headers):
        cell = tbl.cell(0, c)
        cell.fill.solid()
        cell.fill.fore_color.rgb = RGBColor.from_string(theme["accent"])
        run = cell.text_frame.paragraphs[0].add_run()
        run.text = str(h)
        set_run_font(run, size, "FFFFFF", bold=True)
    for r, row in enumerate(rows, start=1):
        for c, val in enumerate(row):
            cell = tbl.cell(r, c)
            cell.fill.solid()
            cell.fill.fore_color.rgb = RGBColor.from_string(zebra if r % 2 == 0 else theme["bg"])
            run = cell.text_frame.paragraphs[0].add_run()
            run.text = str(val)
            set_run_font(run, size, theme["text"])
    if s.get("caption"):
        box = add_textbox(slide, MARGIN, SLIDE_H - Inches(0.6), area_w, Inches(0.35))
        add_para(box.text_frame, s["caption"], tokens["caption"], theme["muted"],
                 first=True, space_before=0)
    return slide


def add_chart_slide(prs, theme, tokens, s, i):
    ct = s.get("chart_type")
    if ct not in ("bar", "line", "pie"):
        fail(f"第 {i} 页 chart_type 必须是 bar/line/pie 之一，收到：{ct!r}")
    cats = s.get("categories")
    series = s.get("series")
    if not s.get("title") or not isinstance(cats, list) or not cats:
        fail(f"第 {i} 页 chart 需要 title 和非空 categories 数组")
    if len(cats) > CHART_MAX_CATEGORIES:
        fail(f"第 {i} 页图表类目数 {len(cats)} 超过 {CHART_MAX_CATEGORIES}，请聚合后重试")
    if not isinstance(series, list) or not series:
        fail(f"第 {i} 页 chart 需要非空 series 数组")
    if len(series) > CHART_MAX_SERIES:
        fail(f"第 {i} 页图表系列数 {len(series)} 超过 {CHART_MAX_SERIES}，请合并次要系列")
    if ct == "pie" and len(series) != 1:
        fail(f"第 {i} 页饼图只支持单系列")
    for se in series:
        if not isinstance(se.get("values"), list) or len(se["values"]) != len(cats):
            fail(f"第 {i} 页系列 {se.get('name')!r} 的 values 数量必须与 categories 一致")
    slide = new_slide(prs, theme)
    header(slide, theme, tokens, s["title"])
    data = CategoryChartData()
    data.categories = cats
    for se in series:
        data.add_series(se.get("name", ""), se["values"])
    xl = {"bar": XL_CHART_TYPE.COLUMN_CLUSTERED,
          "line": XL_CHART_TYPE.LINE_MARKERS,
          "pie": XL_CHART_TYPE.PIE}[ct]
    gf = slide.shapes.add_chart(xl, MARGIN, Inches(1.75), SLIDE_W - MARGIN * 2,
                                SLIDE_H - Inches(2.7), data)
    chart = gf.chart
    chart.has_title = False
    chart.font.size = Pt(tokens["caption"])
    chart.font.color.rgb = RGBColor.from_string(theme["muted"])
    if len(series) > 1 or ct == "pie":
        chart.has_legend = True
        chart.legend.position = XL_LEGEND_POSITION.BOTTOM
        chart.legend.include_in_layout = False
    palette = [theme["accent"], theme.get("accent2", theme["muted"]), theme["muted"],
               blend(theme["accent"], "FFFFFF", 0.45)]
    if ct == "pie":
        for idx, point in enumerate(chart.series[0].points):
            point.format.fill.solid()
            point.format.fill.fore_color.rgb = RGBColor.from_string(palette[idx % len(palette)])
        plot = chart.plots[0]
        plot.has_data_labels = True
        plot.data_labels.font.size = Pt(tokens["caption"])
    else:
        for si, ser in enumerate(chart.series):
            color = RGBColor.from_string(palette[si % len(palette)])
            if ct == "bar":
                ser.format.fill.solid()
                ser.format.fill.fore_color.rgb = color
            else:
                ser.format.line.color.rgb = color
                ser.format.line.width = Pt(2.5)
    if s.get("caption"):
        box = add_textbox(slide, MARGIN, SLIDE_H - Inches(0.6), SLIDE_W - MARGIN * 2, Inches(0.35))
        add_para(box.text_frame, s["caption"], tokens["caption"], theme["muted"],
                 first=True, space_before=0)
    return slide


def build(spec: dict) -> str:
    scenario = spec.get("scenario")
    theme_name = spec.get("theme")
    if scenario not in TOKENS:
        fail(f"scenario 必须是 {sorted(TOKENS)} 之一，收到：{scenario!r}")
    if theme_name not in THEMES:
        fail(f"theme 必须是 {sorted(THEMES)} 之一，收到：{theme_name!r}")
    output = spec.get("output")
    if not output or not output.endswith(".pptx"):
        fail("output 必须是 .pptx 结尾的绝对路径")
    slides_spec = spec.get("slides")
    if not isinstance(slides_spec, list) or not slides_spec:
        fail("slides 必须是非空数组")

    tokens = TOKENS[scenario]
    theme = THEMES[theme_name]
    warnings: list[str] = []

    prs = Presentation()
    prs.slide_width = SLIDE_W
    prs.slide_height = SLIDE_H

    # 展开（拆分要点超限的 bullets 页）后的最终页面列表。
    expanded: list[tuple[dict, str | None]] = []
    for i, s in enumerate(slides_spec, 1):
        layout = s.get("layout")
        check_no_style_overrides(s, i)
        if layout == "bullets":
            if not s.get("title") or not isinstance(s.get("bullets"), list) or not s["bullets"]:
                fail(f"第 {i} 页 bullets 需要 title 和非空 bullets 数组")
            chunks = split_bullets(s["bullets"], i, warnings)
            for j, chunk in enumerate(chunks):
                sub = {**s, "bullets": chunk}
                cont = f"{s['title']}（续）" if j > 0 else None
                expanded.append((sub, cont))
        else:
            expanded.append((s, None))

    for i, (s, cont) in enumerate(expanded, 1):
        layout = s.get("layout")
        if layout == "title":
            if not s.get("title"):
                fail(f"第 {i} 页 title 缺少 title")
            slide = new_slide(prs, theme)
            if theme.get("cover_band"):
                band = slide.shapes.add_shape(1, 0, 0, SLIDE_W, Inches(0.35))
                band.fill.solid()
                band.fill.fore_color.rgb = RGBColor.from_string(theme["accent"])
                band.line.fill.background()
                band.shadow.inherit = False
            box = add_textbox(slide, MARGIN, Inches(2.4), SLIDE_W - MARGIN * 2, Inches(2.5))
            add_para(box.text_frame, s["title"], tokens["title"] + 6, theme["accent"],
                     bold=True, first=True, space_before=0)
            if s.get("subtitle"):
                add_para(box.text_frame, s["subtitle"], tokens["body"], theme["text"], space_before=14)
            if s.get("meta"):
                add_para(box.text_frame, s["meta"], tokens["caption"], theme["muted"], space_before=20)
        elif layout == "section":
            if not s.get("title"):
                fail(f"第 {i} 页 section 缺少 title")
            slide = new_slide(prs, theme)
            box = add_textbox(slide, MARGIN, Inches(3.0), SLIDE_W - MARGIN * 2, Inches(1.8))
            add_para(box.text_frame, s["title"], tokens["title"] + 4, theme["accent"],
                     bold=True, first=True, space_before=0, align=PP_ALIGN.CENTER)
            if s.get("subtitle"):
                add_para(box.text_frame, s["subtitle"], tokens["body"], theme["muted"],
                         space_before=12, align=PP_ALIGN.CENTER)
        elif layout == "bullets":
            slide = add_bullet_slide(prs, theme, tokens, s, i, cont_title=cont)
        elif layout == "two_column":
            left, right = s.get("left"), s.get("right")
            if not s.get("title") or not left or not right:
                fail(f"第 {i} 页 two_column 需要 title、left、right")
            for col_name, col in (("left", left), ("right", right)):
                if not isinstance(col.get("bullets"), list) or not col["bullets"]:
                    fail(f"第 {i} 页 two_column.{col_name}.bullets 必须是非空数组")
                if len(col["bullets"]) > MAX_BULLETS:
                    fail(f"第 {i} 页 {col_name} 栏要点超过 {MAX_BULLETS} 条，请精简或拆页（栏内不自动拆）")
            slide = new_slide(prs, theme)
            header(slide, theme, tokens, s["title"])
            col_w = (SLIDE_W - MARGIN * 2 - Inches(0.5)) // 2
            for c, col in enumerate((left, right)):
                x = MARGIN + c * (col_w + Inches(0.5))
                box = add_textbox(slide, x, Inches(1.75), col_w, Inches(0.6))
                if col.get("heading"):
                    add_para(box.text_frame, col["heading"], tokens["body"], theme["accent"],
                             bold=True, first=True, space_before=0)
                body = add_textbox(slide, x, Inches(2.35), col_w, SLIDE_H - Inches(3.1))
                for j, b in enumerate(col["bullets"]):
                    add_para(body.text_frame, f"{theme['bullet_char']}  {b}", tokens["body"] - 2,
                             theme["text"], first=(j == 0), space_before=8)
        elif layout == "text":
            if not s.get("title") or not isinstance(s.get("paragraphs"), list) or not s["paragraphs"]:
                fail(f"第 {i} 页 text 需要 title 和非空 paragraphs 数组")
            slide = new_slide(prs, theme)
            header(slide, theme, tokens, s["title"])
            box = add_textbox(slide, MARGIN, Inches(1.75), SLIDE_W - MARGIN * 2, SLIDE_H - Inches(2.5))
            for j, para in enumerate(s["paragraphs"]):
                add_para(box.text_frame, para, tokens["body"], theme["text"],
                         first=(j == 0), space_before=12)
            if s.get("notes"):
                slide.notes_slide.notes_text_frame.text = s["notes"]
        elif layout == "image":
            img = s.get("image")
            if not s.get("title") or not img:
                fail(f"第 {i} 页 image 需要 title 和 image（绝对路径）")
            img_path = Path(img)
            if not img_path.is_file():
                fail(f"第 {i} 页图片不存在：{img}")
            slide = new_slide(prs, theme)
            header(slide, theme, tokens, s["title"])
            max_w = SLIDE_W - MARGIN * 2
            max_h = SLIDE_H - Inches(2.6)
            pic = slide.shapes.add_picture(str(img_path), MARGIN, Inches(1.8), width=max_w)
            # 等比缩放到版心内（不拉伸变形），并水平居中。
            scale = min(1.0, max_h / pic.height)
            pic.width = int(pic.width * scale)
            pic.height = int(pic.height * scale)
            pic.left = int((SLIDE_W - pic.width) // 2)
            if s.get("caption"):
                box = add_textbox(slide, MARGIN, pic.top + pic.height + Inches(0.1),
                                  SLIDE_W - MARGIN * 2, Inches(0.4))
                add_para(box.text_frame, s["caption"], tokens["caption"], theme["muted"],
                         first=True, space_before=0, align=PP_ALIGN.CENTER)
        elif layout == "big_number":
            if not s.get("title") or not s.get("number"):
                fail(f"第 {i} 页 big_number 需要 title 和 number")
            slide = new_slide(prs, theme)
            header(slide, theme, tokens, s["title"])
            accent2 = theme.get("accent2", theme["accent"])
            box = add_textbox(slide, MARGIN, Inches(2.6), SLIDE_W - MARGIN * 2, Inches(2.0))
            add_para(box.text_frame, s["number"], min(96, tokens["title"] + 58), accent2,
                     bold=True, first=True, space_before=0, align=PP_ALIGN.CENTER)
            if s.get("label"):
                add_para(box.text_frame, s["label"], tokens["body"], theme["text"],
                         space_before=10, align=PP_ALIGN.CENTER)
            if s.get("notes"):
                slide.notes_slide.notes_text_frame.text = s["notes"]
        elif layout == "closing":
            if not s.get("title"):
                fail(f"第 {i} 页 closing 缺少 title")
            slide = new_slide(prs, theme)
            box = add_textbox(slide, MARGIN, Inches(2.9), SLIDE_W - MARGIN * 2, Inches(1.8))
            add_para(box.text_frame, s["title"], tokens["title"] + 4, theme["accent"],
                     bold=True, first=True, space_before=0, align=PP_ALIGN.CENTER)
            if s.get("subtitle"):
                add_para(box.text_frame, s["subtitle"], tokens["body"], theme["muted"],
                         space_before=12, align=PP_ALIGN.CENTER)
        elif layout == "table":
            slide = add_table_slide(prs, theme, tokens, s, i)
        elif layout == "chart":
            slide = add_chart_slide(prs, theme, tokens, s, i)
        else:
            fail(f"第 {i} 页 layout 未知：{layout!r}（支持 title/section/bullets/two_column/text/image/table/chart/big_number/closing）")

        if layout not in ("title", "closing"):
            footer(slide, theme, tokens, i)

    for w in warnings:
        sys.stderr.write(f"[build_deck][warn] {w}\n")

    out = Path(output)
    out.parent.mkdir(parents=True, exist_ok=True)
    prs.save(str(out))
    return str(out.resolve())


def main() -> None:
    if len(sys.argv) == 2 and sys.argv[1] == "--schema":
        print(json.dumps(SCHEMA, ensure_ascii=False, indent=2))
        return
    if len(sys.argv) != 2:
        fail("用法：python3 build_deck.py deck.json（或 --schema 查看契约）")
    spec_path = Path(sys.argv[1])
    if not spec_path.is_file():
        fail(f"找不到大纲文件：{spec_path}")
    try:
        spec = json.loads(spec_path.read_text(encoding="utf-8"))
    except json.JSONDecodeError as e:
        fail(f"JSON 解析失败：{e}")
    out = build(spec)
    print(out)


if __name__ == "__main__":
    main()
