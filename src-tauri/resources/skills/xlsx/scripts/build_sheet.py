#!/usr/bin/env python3
"""build_sheet.py — 从 JSON 生成符合排版规范的 .xlsx。

用法:
    python3 build_sheet.py sheet.json
    python3 build_sheet.py --schema     # 打印 JSON 契约并退出

排版规范（字体/配色/列宽/数字格式/冻结筛选）固化在本脚本中，JSON 无法覆盖
——这是质量保障机制的一部分。校验失败时退出码为 1，stderr 说明如何修正。
"""

import datetime
import json
import re
import sys
from pathlib import Path

try:
    from openpyxl import Workbook
    from openpyxl.chart import BarChart, LineChart, PieChart, Reference
    from openpyxl.styles import Alignment, Border, Font, PatternFill, Side
    from openpyxl.utils import get_column_letter
except ImportError:
    sys.stderr.write("缺少依赖：openpyxl。请通过 skill_env_setup 或插件页安装隔离环境依赖\n")
    sys.exit(2)

FONT = "微软雅黑"
ACCENTS = {
    "report": {"header": "2F5597", "zebra": "EEF3FA", "title": "1F3864"},
    "ledger": {"header": "2F5597", "zebra": "EEF3FA", "title": "1F3864"},
    "analysis": {"header": "217346", "zebra": "F0F7F2", "title": "1E5B3C"},
}

THIN = Side(style="thin", color="D0D0D0")
CELL_BORDER = Border(left=THIN, right=THIN, top=THIN, bottom=THIN)
SUMMARY_TOP = Border(left=THIN, right=THIN, top=Side(style="double", color="808080"),
                     bottom=THIN)

MAX_SHEETS = 10
MAX_COLS = 30
MAX_ROWS = 10000
MAX_CELL_CHARS = 500
CHART_MAX_SERIES = 4
SUMMARY_FUNCS = {"sum": "SUM", "average": "AVERAGE", "max": "MAX", "min": "MIN",
                 "count": "COUNT"}
FORMULA_RE = re.compile(r"^=[-A-Za-z0-9_()+*/.,:%\s\"']+$")
DATE_RE = re.compile(r"^(\d{4})-(\d{2})-(\d{2})$")
SHEET_NAME_BAD = re.compile(r"[\\/*?\[\]:]")

FORBIDDEN_KEYS = {"font", "font_size", "fontsize", "color", "colour", "bold",
                  "italic", "width", "fill"}

SCHEMA = {
    "template": "report | ledger | analysis（必选）",
    "output": "输出 .xlsx 绝对路径（必选）",
    "title": "工作簿标题（report/analysis 必选；ledger 不使用）",
    "sheets": [{
        "name": "工作表名（≤31 字符，不含 \\ / * ? [ ] :）",
        "title": "表内大标题（可选，缺省用工作簿 title；ledger 不可用）",
        "headers": ["列名（唯一，单位写进表头，如『金额（元）』）"],
        "rows": [["单元格：数字 | 文本 | YYYY-MM-DD 日期 | true/false | null | =公式"]],
        "summary": {"label": "合计（可选，仅 analysis）",
                    "columns": {"表头名": "sum | average | max | min | count"}},
        "chart": {"type": "bar | line | pie（可选，仅 analysis；pie 仅单系列）",
                  "title": "图表标题",
                  "categories": "类目列的表头名",
                  "series": ["系列列的表头名（≤4）"]},
    }],
}


def fail(msg: str) -> None:
    sys.stderr.write(f"[build_sheet] {msg}\n")
    sys.exit(1)


def display_width(text: str) -> int:
    return sum(2 if ord(c) > 0x2E7F else 1 for c in text)


def check_forbidden(obj, path="$"):
    if isinstance(obj, dict):
        for k, v in obj.items():
            if k.lower() in FORBIDDEN_KEYS:
                fail(f"{path}.{k}: 不允许自定义字体/颜色/列宽——排版由模板统一控制")
            check_forbidden(v, f"{path}.{k}")
    elif isinstance(obj, list):
        for i, v in enumerate(obj):
            check_forbidden(v, f"{path}[{i}]")


def validate_sheet(sheet, i, template):
    name = sheet.get("name") or f"Sheet{i + 1}"
    if len(name) > 31 or SHEET_NAME_BAD.search(name):
        fail(f"sheets[{i}].name「{name}」无效（≤31 字符，不含 \\ / * ? [ ] :）")
    headers = sheet.get("headers") or []
    if not 1 <= len(headers) <= MAX_COLS:
        fail(f"sheets[{i}] 列数须为 1-{MAX_COLS}")
    if len(set(headers)) != len(headers):
        fail(f"sheets[{i}] 表头有重复：{headers}")
    for h in headers:
        if not isinstance(h, str) or not h.strip():
            fail(f"sheets[{i}] 表头必须是非空字符串")
    rows = sheet.get("rows")
    if not isinstance(rows, list) or not 1 <= len(rows) <= MAX_ROWS:
        fail(f"sheets[{i}].rows 须为 1-{MAX_ROWS} 行的数组")
    for r, row in enumerate(rows):
        if not isinstance(row, list) or len(row) != len(headers):
            fail(f"sheets[{i}] 第 {r + 1} 行列数与表头（{len(headers)}）不一致")
        for c, v in enumerate(row):
            if v is not None and not isinstance(v, (str, int, float, bool)):
                fail(f"sheets[{i}] 第 {r + 1} 行第 {c + 1} 列类型不支持：{type(v).__name__}")
            if isinstance(v, str):
                if len(v) > MAX_CELL_CHARS:
                    fail(f"sheets[{i}] 第 {r + 1} 行第 {c + 1} 列超过 {MAX_CELL_CHARS} 字")
                if v.startswith("=") and not FORMULA_RE.match(v):
                    fail(f"sheets[{i}] 第 {r + 1} 行第 {c + 1} 列公式含非法字符：{v[:40]}")

    if template == "ledger" and sheet.get("title"):
        fail("ledger（台账）模板不支持大标题——去掉 sheets[].title")
    summary = sheet.get("summary")
    if summary:
        if template != "analysis":
            fail("summary（合计行）仅 analysis 模板支持")
        for col, func in (summary.get("columns") or {}).items():
            if col not in headers:
                fail(f"summary 引用了不存在的表头「{col}」")
            if func not in SUMMARY_FUNCS:
                fail(f"summary 的聚合方式「{func}」无效，允许：{sorted(SUMMARY_FUNCS)}")
    chart = sheet.get("chart")
    if chart:
        if template != "analysis":
            fail("chart（图表）仅 analysis 模板支持")
        ctype = chart.get("type")
        if ctype not in ("bar", "line", "pie"):
            fail(f"chart.type「{ctype}」无效，允许 bar | line | pie")
        if chart.get("categories") not in headers:
            fail(f"chart.categories 引用了不存在的表头「{chart.get('categories')}」")
        series = chart.get("series") or []
        if not 1 <= len(series) <= CHART_MAX_SERIES:
            fail(f"chart.series 数量须为 1-{CHART_MAX_SERIES}")
        if ctype == "pie" and len(series) != 1:
            fail("饼图仅支持单系列")
        for s in series:
            if s not in headers:
                fail(f"chart.series 引用了不存在的表头「{s}」")
    return name, headers, rows


def is_number(v) -> bool:
    return isinstance(v, (int, float)) and not isinstance(v, bool)


def write_cell(cell, value, *, align=None, border=CELL_BORDER, bold=False,
               color=None, size=11, number_format=None, wrap=False):
    if isinstance(value, str) and value.startswith("="):
        cell.value = value
    elif isinstance(value, bool):
        cell.value = "是" if value else "否"
        align = align or Alignment(horizontal="center", vertical="center")
    elif isinstance(value, str):
        m = DATE_RE.match(value)
        if m:
            cell.value = datetime.date(int(m.group(1)), int(m.group(2)), int(m.group(3)))
            cell.number_format = "yyyy-mm-dd"
            align = align or Alignment(horizontal="right", vertical="center")
        else:
            cell.value = value
    elif value is not None:
        cell.value = value
    if number_format and value is not None and not (isinstance(value, str) and value.startswith("=")):
        cell.number_format = number_format
    if align is None:
        if is_number(cell.value) or cell.value is None:
            align = Alignment(horizontal="right", vertical="center")
        else:
            align = Alignment(horizontal="left", vertical="center", wrap_text=wrap)
    cell.alignment = align
    cell.border = border
    cell.font = Font(name=FONT, size=size, bold=bold,
                     color=color or "000000")


def build_sheet(ws, sheet, template, book_title):
    accent = ACCENTS[template]
    headers = sheet["headers"]
    rows = sheet["rows"]
    ncols = len(headers)

    has_title = template != "ledger"
    header_row = 2 if has_title else 1
    data_start = header_row + 1
    data_end = header_row + len(rows)

    if has_title:
        title = (sheet.get("title") or book_title).strip()
        ws.merge_cells(start_row=1, start_column=1, end_row=1, end_column=ncols)
        c = ws.cell(row=1, column=1)
        c.value = title
        c.font = Font(name=FONT, size=16, bold=True, color=accent["title"])
        c.alignment = Alignment(horizontal="center", vertical="center")
        ws.row_dimensions[1].height = 30

    for j, h in enumerate(headers, 1):
        c = ws.cell(row=header_row, column=j)
        c.value = h
        c.font = Font(name=FONT, size=11, bold=True, color="FFFFFF")
        c.fill = PatternFill("solid", fgColor=accent["header"])
        c.alignment = Alignment(horizontal="center", vertical="center", wrap_text=True)
        c.border = CELL_BORDER

    # 每列数字格式：全整数列用千分位无小数，否则两位小数
    col_formats = {}
    for j in range(ncols):
        vals = [row[j] for row in rows if is_number(row[j])]
        if vals:
            col_formats[j] = "#,##0" if all(float(v) == int(v) for v in vals) else "#,##0.00"

    zebra_fill = PatternFill("solid", fgColor=accent["zebra"])
    for i, row in enumerate(rows):
        for j, v in enumerate(row):
            c = ws.cell(row=data_start + i, column=j + 1)
            write_cell(c, v, number_format=col_formats.get(j) if is_number(v) else None)
            if i % 2 == 1:
                c.fill = zebra_fill

    last_row = data_end
    summary = sheet.get("summary")
    if summary:
        last_row = data_end + 1
        label = (summary.get("label") or "合计").strip()
        write_cell(ws.cell(row=last_row, column=1), label, bold=True,
                   border=SUMMARY_TOP,
                   align=Alignment(horizontal="left", vertical="center"))
        col_funcs = summary.get("columns") or {}
        for j, h in enumerate(headers):
            c = ws.cell(row=last_row, column=j + 1)
            func = col_funcs.get(h)
            if func:
                letter = get_column_letter(j + 1)
                rng = f"{letter}{data_start}:{letter}{data_end}"
                if func == "count" and j not in col_formats:
                    formula = f"=COUNTA({rng})"
                else:
                    formula = f"={SUMMARY_FUNCS[func]}({rng})"
                write_cell(c, formula, bold=True, border=SUMMARY_TOP,
                           number_format=col_formats.get(j))
            else:
                write_cell(c, None, border=SUMMARY_TOP)

    # 列宽：按表头与数据内容自动计算（中文按 2 字符宽）
    for j, h in enumerate(headers):
        width = display_width(h)
        for row in rows[:200]:
            v = row[j]
            if v is None:
                continue
            text = str(v) if not isinstance(v, bool) else "是"
            if isinstance(v, str) and v.startswith("="):
                text = "0" * 10
            width = max(width, display_width(text))
        ws.column_dimensions[get_column_letter(j + 1)].width = min(max(width + 2, 8), 40)

    ws.freeze_panes = f"A{data_start}"
    ws.auto_filter.ref = f"A{header_row}:{get_column_letter(ncols)}{data_end}"

    chart = sheet.get("chart")
    if chart:
        classes = {"bar": BarChart, "line": LineChart, "pie": PieChart}
        ch = classes[chart["type"]]()
        ch.title = (chart.get("title") or "").strip() or None
        ch.width, ch.height = 16, 9
        cat_col = headers.index(chart["categories"]) + 1
        cats = Reference(ws, min_col=cat_col, min_row=data_start, max_row=data_end)
        for s in chart["series"]:
            s_col = headers.index(s) + 1
            ref = Reference(ws, min_col=s_col, min_row=header_row, max_row=data_end)
            ch.add_data(ref, titles_from_data=True)
        ch.set_categories(cats)
        anchor = f"{get_column_letter(ncols + 2)}{header_row}"
        ws.add_chart(ch, anchor)


def main() -> None:
    if len(sys.argv) >= 2 and sys.argv[1] == "--schema":
        print(json.dumps(SCHEMA, ensure_ascii=False, indent=2))
        return
    if len(sys.argv) < 2:
        sys.stderr.write("用法：python3 build_sheet.py sheet.json | --schema\n")
        sys.exit(1)
    spec_path = Path(sys.argv[1])
    if not spec_path.is_file():
        fail(f"JSON 文件不存在：{spec_path}")
    try:
        spec = json.loads(spec_path.read_text(encoding="utf-8"))
    except json.JSONDecodeError as e:
        fail(f"JSON 解析失败：{e}")

    template = spec.get("template")
    if template not in ACCENTS:
        fail(f"template「{template}」无效，须为 report | ledger | analysis")
    output = spec.get("output") or ""
    if not Path(output).is_absolute() or not output.lower().endswith(".xlsx"):
        fail(f"output 须为 .xlsx 绝对路径：{output}")
    book_title = (spec.get("title") or "").strip()
    if template != "ledger" and not book_title:
        fail("report/analysis 模板缺少 title（工作簿标题）")

    check_forbidden(spec)
    sheets = spec.get("sheets")
    if not isinstance(sheets, list) or not 1 <= len(sheets) <= MAX_SHEETS:
        fail(f"sheets 须为 1-{MAX_SHEETS} 个工作表的数组")
    names = set()
    for i, sheet in enumerate(sheets):
        name, _, _ = validate_sheet(sheet, i, template)
        if name in names:
            fail(f"工作表名重复：「{name}」")
        names.add(name)

    wb = Workbook()
    wb.remove(wb.active)
    for i, sheet in enumerate(sheets):
        ws = wb.create_sheet(title=sheet.get("name") or f"Sheet{i + 1}")
        build_sheet(ws, sheet, template, book_title)

    out = Path(output)
    out.parent.mkdir(parents=True, exist_ok=True)
    wb.save(str(out))
    print(out)


if __name__ == "__main__":
    main()
