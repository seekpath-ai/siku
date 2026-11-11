#!/usr/bin/env python3
"""build_doc.py — 从 JSON 大纲生成符合格式规范的 .docx。

用法:
    python3 build_doc.py doc.json
    python3 build_doc.py --schema     # 打印 JSON 契约并退出

格式规范（字体/字号/行距/结构）固化在本脚本中，JSON 无法覆盖——这是质量
保障机制的一部分。校验失败时退出码为 1，stderr 说明如何修正。
"""

import json
import re
import sys
from pathlib import Path

try:
    from docx import Document
    from docx.shared import Cm, Pt, RGBColor
    from docx.enum.table import WD_TABLE_ALIGNMENT
    from docx.enum.text import WD_ALIGN_PARAGRAPH
    from docx.oxml import OxmlElement
    from docx.oxml.ns import qn
except ImportError:
    sys.stderr.write("缺少依赖：python-docx。请通过 skill_env_setup 或插件页安装隔离环境依赖\n")
    sys.exit(2)

CENTER = WD_ALIGN_PARAGRAPH.CENTER
RIGHT = WD_ALIGN_PARAGRAPH.RIGHT
JUSTIFY = WD_ALIGN_PARAGRAPH.JUSTIFY

# ── 模板令牌（与 references/*.md 一致，修改需同步） ──

FANGSONG = "仿宋_GB2312"      # 公文正文；查看器缺字体时回退仿宋
KAITI = "楷体_GB2312"         # 公文二级标题；缺字体时回退楷体
HEITI = "黑体"
XIAOBIAOSONG = "方正小标宋简体"
SONGTI = "宋体"
YAHEI = "微软雅黑"
TIMES = "Times New Roman"
CALIBRI = "Calibri"

RED = "FF0000"
GRAY = "808080"

# 字号（磅）：二号 22 / 小三 15 / 三号 16 / 四号 14 / 小四 12 / 五号 10.5
TEMPLATES = {
    "official": {  # GB/T 9704-2012
        "margins_cm": (3.7, 3.5, 2.8, 2.6),
        "body_cjk": FANGSONG, "body_latin": TIMES, "body_size": 16,
        "line_exact": 28.8,
    },
    "thesis": {
        "margins_cm": (2.54, 2.54, 3.17, 3.17),
        "body_cjk": SONGTI, "body_latin": TIMES, "body_size": 12,
        "line_multiple": 1.5,
    },
    "enterprise": {
        "margins_cm": (2.54, 2.54, 3.17, 3.17),
        "body_cjk": SONGTI, "body_latin": CALIBRI, "body_size": 12,
        "line_multiple": 1.5,
    },
}

TABLE_MAX_COLS = 8
TABLE_MAX_ROWS = 50
LIST_MAX_ITEMS = 20
WARN_PARA_CHARS = 500

FORBIDDEN_KEYS = {"font", "font_size", "fontsize", "color", "colour", "bold", "italic"}

CN_DATE_RE = re.compile(r"^(\d{4})年(\d{1,2})月(\d{1,2})日$")
ISO_DATE_RE = re.compile(r"^(\d{4})-(\d{1,2})-(\d{1,2})$")
CN_NUM = "零一二三四五六七八九"

SCHEMA = {
    "template": "official | thesis | enterprise（必选）",
    "output": "输出 .docx 绝对路径（必选）",
    "title": "文档标题（必选）",
    "meta": {
        "official": {
            "org": "发文机关标志，如『××市教育局文件』（可选，给了才生成红头）",
            "doc_number": "发文字号，如『×教发〔2026〕5号』（可选，须含〔〕）",
            "copy_number": "份号，如 000001（可选）",
            "secrecy": "密级，如『秘密★1年』（可选）",
            "urgency": "紧急程度，如『特急』（可选）",
            "recipients": "主送机关（必选），字符串或数组，末尾须带全角冒号",
            "attachments": ["附件名称（可选）"],
            "issuer": "发文机关署名（可选，缺省用 org 去掉『文件』）",
            "date": "成文日期 YYYY年M月D日 或 YYYY-MM-DD（必选）",
            "cc": "抄送机关（可选）",
        },
        "thesis": {
            "authors": ["作者1", "作者2"],
            "affiliation": "（××大学 ××学院，城市 邮编）",
            "date": "日期（可选）",
        },
        "enterprise": {
            "org": "单位抬头（可选）",
            "doc_number": "文件编号（可选）",
            "issuer": "落款单位（可选）",
            "date": "落款日期 YYYY年M月D日 或 YYYY-MM-DD（可选）",
        },
    },
    "blocks": [
        {"type": "h1 | h2 | h3 | h4", "text": "（编号脚本自动加，不要手写；enterprise 不自动编号、h4 仅 official）"},
        {"type": "para", "text": "..."},
        {"type": "list", "items": ["..."]},
        {"type": "table", "caption": "表题（thesis 必填）", "headers": ["列1"], "rows": [["..."]]},
        {"type": "figure", "path": "绝对路径", "caption": "图题（thesis 必填）"},
        {"type": "abstract", "text": "（thesis 必填，仅一次）"},
        {"type": "keywords", "items": ["3-5 个（thesis 必填，仅一次）"]},
        {"type": "references", "items": ["GB/T 7714 条目（thesis 可选，仅一次）"]},
        {"type": "page_break"},
    ],
}


def fail(msg: str) -> None:
    sys.stderr.write(f"[build_doc] {msg}\n")
    sys.exit(1)


def warn(msg: str) -> None:
    sys.stderr.write(f"[build_doc warn] {msg}\n")


def cn_num(n: int) -> str:
    if n <= 10:
        return ["", "一", "二", "三", "四", "五", "六", "七", "八", "九", "十"][n]
    if n < 20:
        return "十" + CN_NUM[n % 10]
    if n > 99:
        return str(n)
    tens, ones = divmod(n, 10)
    return CN_NUM[tens] + "十" + (CN_NUM[ones] if ones else "")


# ── 底层排版工具 ──

def set_run_font(run, latin: str, cjk: str, size_pt: float, bold: bool = False, color: str = None) -> None:
    """同时设置西文与东亚字体（python-docx 默认只写 ascii/hAnsi）。"""
    run.font.name = latin
    run.font.size = Pt(size_pt)
    run.font.bold = bold
    if color:
        run.font.color.rgb = RGBColor.from_string(color)
    rPr = run._r.get_or_add_rPr()
    rFonts = rPr.find(qn("w:rFonts"))
    if rFonts is None:
        rFonts = OxmlElement("w:rFonts")
        rPr.insert(0, rFonts)
    rFonts.set(qn("w:ascii"), latin)
    rFonts.set(qn("w:hAnsi"), latin)
    rFonts.set(qn("w:eastAsia"), cjk)


def set_bottom_border(paragraph, color: str, sz: int = 12) -> None:
    """段落下框线。sz 单位为 1/8 磅（sz=12 即 1.5 磅）。"""
    pPr = paragraph._p.get_or_add_pPr()
    pBdr = OxmlElement("w:pBdr")
    bottom = OxmlElement("w:bottom")
    bottom.set(qn("w:val"), "single")
    bottom.set(qn("w:sz"), str(sz))
    bottom.set(qn("w:space"), "1")
    bottom.set(qn("w:color"), color)
    pBdr.append(bottom)
    pPr.insert_element_before(
        pBdr,
        "w:shd", "w:tabs", "w:suppressAutoHyphens", "w:kinsoku", "w:wordWrap",
        "w:overflowPunct", "w:topLinePunct", "w:autoSpaceDE", "w:autoSpaceDN",
        "w:bidi", "w:adjustRightInd", "w:snapToGrid", "w:spacing", "w:ind",
        "w:contextualSpacing", "w:mirrorIndents", "w:suppressOverlap", "w:jc",
        "w:textDirection", "w:textAlignment", "w:textboxTightWrap",
        "w:outlineLvl", "w:divId", "w:cnfStyle", "w:rPr", "w:sectPr", "w:pPrChange",
    )


def set_first_line_indent(paragraph, size_pt: float, chars: int = 2) -> None:
    """首行缩进 chars 个字符。w:firstLineChars 随字号缩放（Word/WPS），
    w:firstLine 给只认绝对值的渲染器（LibreOffice）兜底，两者同时写入。"""
    ind = paragraph._p.get_or_add_pPr().get_or_add_ind()
    ind.set(qn("w:firstLineChars"), str(chars * 100))
    ind.set(qn("w:firstLine"), str(int(size_pt * chars * 20)))  # 缇（1/20 磅）


def add_par(doc, text, latin, cjk, size, *, bold=False, color=None, align=None,
            indent_chars=0, line_exact=None, line_multiple=None,
            space_before=0, space_after=0, right_indent_pt=0):
    p = doc.add_paragraph()
    if align is not None:
        p.alignment = align
    pf = p.paragraph_format
    if space_before:
        pf.space_before = Pt(space_before)
    if space_after:
        pf.space_after = Pt(space_after)
    if right_indent_pt:
        pf.right_indent = Pt(right_indent_pt)
    if line_exact:
        pf.line_spacing = Pt(line_exact)  # Length 值写入 lineRule="exact"
    elif line_multiple:
        pf.line_spacing = line_multiple
    if indent_chars:
        set_first_line_indent(p, size, indent_chars)
    if text:
        run = p.add_run(text)
        set_run_font(run, latin, cjk, size, bold=bold, color=color)
    return p


def blank_line(doc, size: float):
    """与正文字号一致的空行，用于「下空一行」式间距。"""
    p = doc.add_paragraph()
    pPr = p._p.get_or_add_pPr()
    rPr = OxmlElement("w:rPr")
    sz = OxmlElement("w:sz")
    sz.set(qn("w:val"), str(int(size * 2)))  # 半磅为单位
    rPr.append(sz)
    pPr.append(rPr)
    return p


def add_page_field_run(paragraph, latin, cjk, size):
    run = paragraph.add_run()
    set_run_font(run, latin, cjk, size)
    fld_begin = OxmlElement("w:fldChar")
    fld_begin.set(qn("w:fldCharType"), "begin")
    instr = OxmlElement("w:instrText")
    instr.set(qn("xml:space"), "preserve")
    instr.text = " PAGE "
    fld_end = OxmlElement("w:fldChar")
    fld_end.set(qn("w:fldCharType"), "end")
    run._r.append(fld_begin)
    run._r.append(instr)
    run._r.append(fld_end)


def setup_footer(doc, *, dashes: bool, latin: str, cjk: str, size: float) -> None:
    p = doc.sections[0].footer.paragraphs[0]
    p.alignment = CENTER
    if dashes:
        set_run_font(p.add_run("— "), latin, cjk, size)
    add_page_field_run(p, latin, cjk, size)
    if dashes:
        set_run_font(p.add_run(" —"), latin, cjk, size)


def setup_page(doc, margins_cm) -> None:
    sec = doc.sections[0]
    top, bottom, left, right = margins_cm
    sec.page_width, sec.page_height = Cm(21.0), Cm(29.7)
    sec.top_margin, sec.bottom_margin = Cm(top), Cm(bottom)
    sec.left_margin, sec.right_margin = Cm(left), Cm(right)


def fill_cell(cell, text, *, latin, cjk, size, bold=False, align=CENTER) -> None:
    p = cell.paragraphs[0]
    p.alignment = align
    run = p.add_run(text)
    set_run_font(run, latin, cjk, size, bold=bold)


def add_table_block(doc, headers, rows, *, latin, cjk, size) -> None:
    table = doc.add_table(rows=len(rows) + 1, cols=len(headers))
    table.style = "Table Grid"
    table.alignment = WD_TABLE_ALIGNMENT.CENTER
    for j, h in enumerate(headers):
        fill_cell(table.rows[0].cells[j], h, latin=latin, cjk=cjk, size=size, bold=True)
    for i, row in enumerate(rows):
        for j, val in enumerate(row):
            fill_cell(table.rows[i + 1].cells[j], val, latin=latin, cjk=cjk, size=size)


def add_figure_block(doc, path, *, width_cm=12.0) -> None:
    p = doc.add_paragraph()
    p.alignment = CENTER
    p.add_run().add_picture(path, width=Cm(width_cm))


# ── 校验 ──

def check_forbidden(obj, path="$"):
    if isinstance(obj, dict):
        for k, v in obj.items():
            if k.lower() in FORBIDDEN_KEYS:
                fail(f"{path}.{k}: 不允许自定义字体/字号/颜色——排版由模板统一控制")
            check_forbidden(v, f"{path}.{k}")
    elif isinstance(obj, list):
        for i, v in enumerate(obj):
            check_forbidden(v, f"{path}[{i}]")


def normalize_date(value, field: str) -> str:
    m = CN_DATE_RE.match(value) or ISO_DATE_RE.match(value)
    if not m:
        fail(f"meta.{field}: 日期格式应为 YYYY年M月D日 或 YYYY-MM-DD，得到「{value}」")
    return f"{m.group(1)}年{int(m.group(2))}月{int(m.group(3))}日"


def check_table(block):
    headers = block.get("headers") or []
    rows = block.get("rows") or []
    if not headers:
        fail("table 块缺少 headers")
    if len(headers) > TABLE_MAX_COLS:
        fail(f"table 列数 {len(headers)} 超过上限 {TABLE_MAX_COLS}——拆成多张表")
    if len(rows) > TABLE_MAX_ROWS:
        fail(f"table 行数 {len(rows)} 超过上限 {TABLE_MAX_ROWS}——拆成多张表")
    for i, row in enumerate(rows):
        if len(row) != len(headers):
            fail(f"table 第 {i + 1} 行列数（{len(row)}）与表头（{len(headers)}）不一致")


def check_figure(block):
    path = block.get("path") or ""
    if not path:
        fail("figure 块缺少 path")
    if not Path(path).is_absolute():
        fail(f"figure.path 须为绝对路径：{path}")
    if not Path(path).is_file():
        fail(f"figure.path 文件不存在：{path}")


def validate_blocks(spec):
    blocks = spec.get("blocks")
    if not isinstance(blocks, list) or not blocks:
        fail("blocks 必须是非空数组")
    allowed = {"h1", "h2", "h3", "h4", "para", "list", "table", "figure",
               "abstract", "keywords", "references", "page_break"}
    seen_h1 = False
    counts = {}
    for i, block in enumerate(blocks):
        if not isinstance(block, dict) or "type" not in block:
            fail(f"blocks[{i}] 缺少 type")
        t = block["type"]
        if t not in allowed:
            fail(f"blocks[{i}].type「{t}」无效，允许值：{sorted(allowed)}")
        counts[t] = counts.get(t, 0) + 1
        if t == "h1":
            seen_h1 = True
        if t in ("h2", "h3", "h4") and not seen_h1 and spec["template"] != "enterprise":
            fail(f"blocks[{i}]（{t}）出现在任何 h1 之前——层次标题不得越级")
        if t in ("h1", "h2", "h3", "h4") and not (block.get("text") or "").strip():
            fail(f"blocks[{i}]（{t}）缺少 text")
        if t == "para":
            text = (block.get("text") or "").strip()
            if not text:
                fail(f"blocks[{i}]（para）text 为空")
            if len(text) > WARN_PARA_CHARS:
                warn(f"blocks[{i}]（para）{len(text)} 字偏长，考虑拆段")
        if t == "list":
            items = block.get("items") or []
            if not items or len(items) > LIST_MAX_ITEMS:
                fail(f"blocks[{i}]（list）items 数量须为 1-{LIST_MAX_ITEMS}")
        if t == "table":
            check_table(block)
        if t == "figure":
            check_figure(block)
    for once in ("abstract", "keywords", "references"):
        if counts.get(once, 0) > 1:
            fail(f"{once} 块只能出现一次")
    return blocks


# ── 正文块渲染（按模板分派） ──

def render_body_blocks(doc, blocks, template) -> None:
    cfg = TEMPLATES[template]
    line_kw = {"line_exact": cfg["line_exact"]} if "line_exact" in cfg else {"line_multiple": cfg["line_multiple"]}
    n1 = n2 = n3 = n4 = fig_no = tab_no = 0

    def body(text, **kw):
        add_par(doc, text, cfg["body_latin"], cfg["body_cjk"], cfg["body_size"],
                align=JUSTIFY, indent_chars=2, **{**line_kw, **kw})

    for block in blocks:
        t = block["type"]
        text = (block.get("text") or "").strip()

        if t == "para":
            body(text)
        elif t == "page_break":
            doc.add_page_break()
        elif t == "h1":
            n1 += 1
            n2 = n3 = n4 = 0
            if template == "official":
                if not re.match(r"^[一二三四五六七八九十百]+、", text):
                    text = f"{cn_num(n1)}、{text}"
                add_par(doc, text, TIMES, HEITI, 16, indent_chars=2, **line_kw)
            elif template == "thesis":
                if not re.match(r"^\d", text):
                    text = f"{n1}  {text}"
                add_par(doc, text, TIMES, HEITI, 16, space_before=10, space_after=6, **line_kw)
            else:
                add_par(doc, text, CALIBRI, YAHEI, 14, bold=True, space_before=10, space_after=6, **line_kw)
        elif t == "h2":
            n2 += 1
            n3 = n4 = 0
            if template == "official":
                if not re.match(r"^（[一二三四五六七八九十百]+）", text):
                    text = f"（{cn_num(n2)}）{text}"
                add_par(doc, text, TIMES, KAITI, 16, indent_chars=2, **line_kw)
            elif template == "thesis":
                if not re.match(r"^\d+\.\d+", text):
                    text = f"{n1}.{n2}  {text}"
                add_par(doc, text, TIMES, HEITI, 14, space_before=8, space_after=4, **line_kw)
            else:
                add_par(doc, text, CALIBRI, YAHEI, 12, bold=True, space_before=8, space_after=4, **line_kw)
        elif t == "h3":
            n3 += 1
            n4 = 0
            if template == "official":
                if not re.match(r"^\d+\.", text):
                    text = f"{n3}.{text}"
                add_par(doc, text, TIMES, FANGSONG, 16, bold=True, indent_chars=2, **line_kw)
            elif template == "thesis":
                if not re.match(r"^\d+\.\d+\.\d+", text):
                    text = f"{n1}.{n2}.{n3}  {text}"
                add_par(doc, text, TIMES, HEITI, 12, space_before=6, space_after=3, **line_kw)
            else:
                add_par(doc, text, CALIBRI, YAHEI, 12, bold=True, space_before=6, space_after=3, **line_kw)
        elif t == "h4":
            if template != "official":
                fail("h4 层次标题仅 official（公文）模板支持")
            n4 += 1
            if not re.match(r"^（\d+）", text):
                text = f"（{n4}）{text}"
            add_par(doc, text, TIMES, FANGSONG, 16, indent_chars=2, **line_kw)
        elif t == "list":
            if template == "official":
                fail("公文不使用项目符号列表——请改用层次标题（一、（一）1.）或分段表达")
            for item in block["items"]:
                add_par(doc, f"• {item}", cfg["body_latin"], cfg["body_cjk"], cfg["body_size"],
                        indent_chars=2, **line_kw)
        elif t == "table":
            caption = (block.get("caption") or "").strip()
            if template == "thesis":
                if not caption:
                    fail("thesis 模板的 table 块必须提供 caption（表题）")
                tab_no += 1
                add_par(doc, f"表{tab_no}  {caption}", TIMES, SONGTI, 10.5, align=CENTER,
                        space_before=6)
            elif caption:
                cap_cjk = FANGSONG if template == "official" else SONGTI
                add_par(doc, caption, cfg["body_latin"], cap_cjk, 12, bold=True, align=CENTER,
                        space_before=6)
            size = 14 if template == "official" else 10.5
            add_table_block(doc, block["headers"], block["rows"],
                            latin=cfg["body_latin"], cjk=cfg["body_cjk"], size=size)
            blank_line(doc, cfg["body_size"])
        elif t == "figure":
            add_figure_block(doc, block["path"])
            caption = (block.get("caption") or "").strip()
            if template == "thesis":
                if not caption:
                    fail("thesis 模板的 figure 块必须提供 caption（图题）")
                fig_no += 1
                add_par(doc, f"图{fig_no}  {caption}", TIMES, SONGTI, 10.5, align=CENTER,
                        space_after=6)
            elif caption:
                cap_cjk = KAITI if template == "official" else SONGTI
                add_par(doc, caption, cfg["body_latin"], cap_cjk, 12, align=CENTER, space_after=6)
        elif t in ("abstract", "keywords", "references"):
            pass  # 由 thesis 构建器单独处理
        else:
            fail(f"块类型「{t}」在此模板中不受支持")


# ── 模板构建器 ──

def build_official(doc, spec) -> None:
    meta = spec.get("meta") or {}
    if not meta.get("recipients"):
        fail("official 模板缺少 meta.recipients（主送机关）")
    if not meta.get("date"):
        fail("official 模板缺少 meta.date（成文日期）")
    recipients = meta["recipients"]
    recipients = [recipients] if isinstance(recipients, str) else list(recipients)
    for r in recipients:
        if not r.rstrip().endswith("："):
            fail(f"主送机关「{r}」末尾须用全角冒号（：）")
    doc_number = meta.get("doc_number")
    if doc_number and not ("〔" in doc_number and "〕" in doc_number):
        fail(f"发文字号「{doc_number}」年份须用六角括号〔〕，如『×教发〔2026〕5号』")
    date = normalize_date(meta["date"], "date")

    setup_page(doc, TEMPLATES["official"]["margins_cm"])
    setup_footer(doc, dashes=True, latin=TIMES, cjk=SONGTI, size=14)

    for key in ("copy_number", "secrecy", "urgency"):
        if meta.get(key):
            add_par(doc, meta[key], TIMES, HEITI, 16, line_exact=28.8)

    org = (meta.get("org") or "").strip()
    if org:
        size = 60 if len(org) <= 6 else 50 if len(org) <= 10 else 40 if len(org) <= 16 else 32
        add_par(doc, org, TIMES, XIAOBIAOSONG, size, color=RED, align=CENTER,
                space_before=12, space_after=12)
    if doc_number:
        add_par(doc, doc_number, TIMES, FANGSONG, 16, align=CENTER, line_exact=28.8)
    if org or doc_number:
        line_p = add_par(doc, "", TIMES, FANGSONG, 16, line_exact=28.8)
        set_bottom_border(line_p, color=RED, sz=12)
        blank_line(doc, 16)

    add_par(doc, spec["title"], TIMES, XIAOBIAOSONG, 22, align=CENTER,
            line_exact=33, space_after=6)
    for r in recipients:
        add_par(doc, r, TIMES, FANGSONG, 16, line_exact=28.8)

    render_body_blocks(doc, spec["blocks"], "official")

    attachments = meta.get("attachments") or []
    if attachments:
        blank_line(doc, 16)
        if len(attachments) == 1:
            add_par(doc, f"附件：{attachments[0]}", TIMES, FANGSONG, 16,
                    indent_chars=2, line_exact=28.8)
        else:
            add_par(doc, f"附件：1. {attachments[0]}", TIMES, FANGSONG, 16,
                    indent_chars=2, line_exact=28.8)
            for i, att in enumerate(attachments[1:], 2):
                add_par(doc, f"{i}. {att}", TIMES, FANGSONG, 16,
                        indent_chars=2, line_exact=28.8)

    blank_line(doc, 16)
    issuer = (meta.get("issuer") or org.removesuffix("文件")).strip()
    if issuer:
        add_par(doc, issuer, TIMES, FANGSONG, 16, align=RIGHT,
                right_indent_pt=64, line_exact=28.8)
    add_par(doc, date, TIMES, FANGSONG, 16, align=RIGHT,
            right_indent_pt=64, line_exact=28.8)

    if meta.get("cc"):
        blank_line(doc, 16)
        add_par(doc, f"抄送：{meta['cc']}", TIMES, FANGSONG, 14, line_exact=28.8)


def build_thesis(doc, spec) -> None:
    meta = spec.get("meta") or {}
    blocks = spec["blocks"]
    kinds = {b["type"] for b in blocks}
    if "abstract" not in kinds:
        fail("thesis 模板缺少 abstract 块（论文必须有摘要）")
    if "keywords" not in kinds:
        fail("thesis 模板缺少 keywords 块（3-5 个关键词）")

    setup_page(doc, TEMPLATES["thesis"]["margins_cm"])
    setup_footer(doc, dashes=False, latin=TIMES, cjk=SONGTI, size=10.5)

    add_par(doc, spec["title"], TIMES, HEITI, 22, align=CENTER, space_after=12)
    authors = meta.get("authors") or []
    if authors:
        add_par(doc, "  ".join(authors), TIMES, SONGTI, 12, align=CENTER, space_after=4)
    if meta.get("affiliation"):
        add_par(doc, meta["affiliation"], TIMES, SONGTI, 10.5, align=CENTER, space_after=4)
    if meta.get("date"):
        add_par(doc, meta["date"], TIMES, SONGTI, 10.5, align=CENTER, space_after=4)
    blank_line(doc, 12)

    front, body_blocks, tail = [], [], []
    for b in blocks:
        if b["type"] in ("abstract", "keywords"):
            front.append(b)
        elif b["type"] == "references":
            tail.append(b)
        else:
            body_blocks.append(b)

    for b in front:
        if b["type"] == "abstract":
            text = (b.get("text") or "").strip()
            if not text:
                fail("abstract 块 text 为空")
            if len(text) > 800:
                warn(f"摘要 {len(text)} 字偏长（建议 200-400 字）")
            add_par(doc, "摘  要", TIMES, HEITI, 15, align=CENTER, space_before=6, space_after=6)
            add_par(doc, text, TIMES, SONGTI, 12, align=JUSTIFY, indent_chars=2,
                    line_multiple=1.5, space_after=6)
        else:
            items = [str(x).strip() for x in (b.get("items") or []) if str(x).strip()]
            if not 2 <= len(items) <= 8:
                fail(f"keywords 须为 2-8 个（当前 {len(items)} 个）")
            p = doc.add_paragraph()
            p.paragraph_format.line_spacing = 1.5
            p.paragraph_format.space_after = Pt(6)
            set_run_font(p.add_run("关键词："), TIMES, HEITI, 12, bold=True)
            set_run_font(p.add_run("；".join(items)), TIMES, SONGTI, 12)
    blank_line(doc, 12)

    render_body_blocks(doc, body_blocks, "thesis")

    for b in tail:
        items = [str(x).strip() for x in (b.get("items") or []) if str(x).strip()]
        if not items:
            fail("references 块 items 为空")
        add_par(doc, "参考文献", TIMES, HEITI, 16, align=CENTER,
                space_before=12, space_after=6)
        for i, item in enumerate(items, 1):
            p = add_par(doc, f"[{i}] {item}", TIMES, SONGTI, 10.5, line_multiple=1.5)
            p.paragraph_format.left_indent = Pt(24)
            p.paragraph_format.first_line_indent = Pt(-24)


def build_enterprise(doc, spec) -> None:
    meta = spec.get("meta") or {}
    setup_page(doc, TEMPLATES["enterprise"]["margins_cm"])
    setup_footer(doc, dashes=False, latin=CALIBRI, cjk=SONGTI, size=10.5)

    if meta.get("org"):
        p = add_par(doc, meta["org"], CALIBRI, YAHEI, 16, bold=True, align=CENTER,
                    space_after=4)
        set_bottom_border(p, color=GRAY, sz=6)
    if meta.get("doc_number"):
        add_par(doc, meta["doc_number"], CALIBRI, SONGTI, 10.5, align=CENTER, space_after=4)
    add_par(doc, spec["title"], CALIBRI, YAHEI, 16, bold=True, align=CENTER,
            space_before=12, space_after=12)

    render_body_blocks(doc, spec["blocks"], "enterprise")

    issuer = (meta.get("issuer") or "").strip()
    date = normalize_date(meta["date"], "date") if meta.get("date") else ""
    if issuer or date:
        blank_line(doc, 12)
        if issuer:
            add_par(doc, issuer, CALIBRI, SONGTI, 12, align=RIGHT)
        if date:
            add_par(doc, date, CALIBRI, SONGTI, 12, align=RIGHT)


BUILDERS = {"official": build_official, "thesis": build_thesis, "enterprise": build_enterprise}


def main() -> None:
    if len(sys.argv) >= 2 and sys.argv[1] == "--schema":
        print(json.dumps(SCHEMA, ensure_ascii=False, indent=2))
        return
    if len(sys.argv) < 2:
        sys.stderr.write("用法：python3 build_doc.py doc.json | --schema\n")
        sys.exit(1)
    spec_path = Path(sys.argv[1])
    if not spec_path.is_file():
        fail(f"JSON 文件不存在：{spec_path}")
    try:
        spec = json.loads(spec_path.read_text(encoding="utf-8"))
    except json.JSONDecodeError as e:
        fail(f"JSON 解析失败：{e}")

    template = spec.get("template")
    if template not in BUILDERS:
        fail(f"template「{template}」无效，须为 official | thesis | enterprise")
    output = spec.get("output") or ""
    if not Path(output).is_absolute() or not output.lower().endswith(".docx"):
        fail(f"output 须为 .docx 绝对路径：{output}")
    if not (spec.get("title") or "").strip():
        fail("缺少 title（文档标题）")

    check_forbidden(spec)
    spec["blocks"] = validate_blocks(spec)

    doc = Document()
    BUILDERS[template](doc, spec)
    out = Path(output)
    out.parent.mkdir(parents=True, exist_ok=True)
    doc.save(str(out))
    print(out)


if __name__ == "__main__":
    main()
