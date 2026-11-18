#!/usr/bin/env python3
"""paper_figure.py — 从 JSON 规格生成论文级矢量图表。

用法:
    python3 paper_figure.py --schema
    python3 paper_figure.py figure-spec.json

输出（写到 spec.output_dir）:
    figure.pdf / figure.svg / preview.png
    render.py / spec.json / README.md

依赖: matplotlib（装在工作区虚拟环境，不装系统 Python）。
"""

from __future__ import annotations

import argparse
import csv
import json
import math
import re
import shutil
import subprocess
import sys
from pathlib import Path
from typing import Any

DEFAULT_COLORS = [
    "#0072B2", "#D55E00", "#009E73", "#CC79A7",
    "#E69F00", "#56B4E9", "#F0E442", "#000000",
]
MARKERS = ["o", "s", "^", "D", "v", "P", "X", "*"]
LINESTYLES = ["-", "--", "-.", ":"]

SCHEMA: dict[str, Any] = {
    "output_dir": "绝对路径；输出图表包写到该目录",
    "meta": {
        "title": "可选：图表标题（写进 README）",
        "data_source": "可选但强烈建议：数据来源",
        "venue": "可选：目标会议/期刊",
        "notes": "可选：补充说明",
    },
    "figure": {
        "width_in": "3.5（单栏）或 7.0（双栏）",
        "height_in": "建议 2.0–3.0",
        "font_size": "7–9（pt）",
        "font_family": "可选，默认 DejaVu Sans",
        "dpi": "可选，预览 PNG 的 DPI，默认 600",
    },
    "layout": {"cols": "可选，默认 min(2, panels 数)"},
    "panels": [
        {
            "kind": "bar | grouped_bar | line | scatter | box | violin | heatmap",
            "title": "可选子图标题",
            "label": "可选子图编号，如 a；标题显示为 (a) ...",
            "xlabel": "可选",
            "ylabel": "可选",
            "legend": "可选，默认 true",
            "grid": "可选，默认 line/scatter 为 true，其余 false",
            "colors": ["可选颜色数组"],
            "categories": "bar/grouped_bar 必填：类目数组",
            "data_file": "可选：CSV/TSV/JSON 数据文件；相对 spec 文件目录或绝对路径",
            "data": {"x": "数据文件的 x/类目列", "y": ["数值列"], "group": "box/violin 分组列", "value": "box/violin 数值列"},
            "series": [
                {
                    "name": "系列名",
                    "values": "bar 必填：数值数组",
                    "x": "line/scatter 可选：x 数组",
                    "y": "line/scatter 必填：y 数组",
                    "yerr": "line 可选：误差数组",
                    "size": "scatter 可选：点大小",
                }
            ],
            "groups": [
                {"name": "组名", "values": "box/violin 必填：数值数组"}
            ],
            "matrix": "heatmap 必填：二维数值数组",
            "xlabels": "heatmap 可选",
            "ylabels": "heatmap 可选",
            "cmap": "heatmap 可选，默认 viridis",
            "annotate": "heatmap 可选：是否在格子里写数值",
            "annotations": "可选：显著性标注 [{x1,x2,y,text}]",
        }
    ],
}

MAX_PANELS = 12
WIDTH_RANGE = (1.5, 12.0)
HEIGHT_RANGE = (1.2, 12.0)
FONT_RANGE = (6.0, 18.0)
DPI_RANGE = (150, 1200)


def fail(message: str) -> None:
    sys.stderr.write(message.rstrip() + "\n")
    raise SystemExit(1)


def warn(warnings: list[str], message: str) -> None:
    warnings.append(message)


def load_spec(path: Path) -> dict[str, Any]:
    try:
        spec = json.loads(path.read_text(encoding="utf-8"))
    except OSError as exc:
        fail(f"无法读取规格文件：{exc}")
    except json.JSONDecodeError as exc:
        fail(f"规格文件不是合法 JSON：{exc}")
    if not isinstance(spec, dict):
        fail("规格必须是 JSON 对象")
    return spec


def require_number(value: Any, name: str) -> float:
    if not isinstance(value, (int, float)) or isinstance(value, bool):
        fail(f"{name} 必须是数字")
    number = float(value)
    if not math.isfinite(number):
        fail(f"{name} 不能是 NaN/Infinity")
    return number


def number_list(value: Any, name: str) -> list[float]:
    if not isinstance(value, list) or not value:
        fail(f"{name} 必须是非空数字数组")
    return [require_number(v, f"{name}[{i}]") for i, v in enumerate(value)]


def string_list(value: Any, name: str) -> list[str]:
    if not isinstance(value, list) or not value:
        fail(f"{name} 必须是非空字符串数组")
    return [str(v) for v in value]


def load_table(path: Path) -> list[dict[str, Any]]:
    suffix = path.suffix.lower()
    if suffix in {".csv", ".tsv"}:
        delimiter = "\t" if suffix == ".tsv" else ","
        try:
            with path.open("r", encoding="utf-8-sig", newline="") as handle:
                return [dict(row) for row in csv.DictReader(handle, delimiter=delimiter)]
        except OSError as exc:
            fail(f"无法读取数据文件 {path}：{exc}")
    if suffix == ".json":
        try:
            payload = json.loads(path.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError) as exc:
            fail(f"无法读取数据文件 {path}：{exc}")
        if not isinstance(payload, list) or not all(isinstance(row, dict) for row in payload):
            fail(f"JSON 数据文件必须是由对象组成的数组：{path}")
        return payload
    fail(f"不支持的数据文件类型：{path.suffix}（支持 .csv/.tsv/.json）")


def as_number(value: Any, context: str) -> float:
    try:
        n = float(value)
    except (TypeError, ValueError):
        fail(f"{context} 不是数字：{value!r}")
    if not math.isfinite(n):
        fail(f"{context} 不能是 NaN/Infinity")
    return n


def materialize_data(spec: dict[str, Any], spec_dir: Path) -> None:
    """Expand `data_file` references into the panel arrays used by the renderer.

    Wide-table convention: one x column, one or more y columns. Box/violin use
    one group and one value column. The model never needs to transcribe the
    numbers into JSON.
    """
    for i, panel in enumerate(spec.get("panels", [])):
        data_file = panel.get("data_file")
        if not data_file:
            continue
        if not isinstance(data_file, str):
            fail(f"panels[{i}].data_file 必须是字符串")
        path = Path(data_file)
        if not path.is_absolute():
            path = spec_dir / path
        rows = load_table(path)
        if not rows:
            fail(f"panels[{i}].data_file 为空：{path}")
        mapping = panel.get("data")
        if not isinstance(mapping, dict):
            fail(f"panels[{i}].data 必须描述 x/y 或 group/value 列")
        kind = panel.get("kind")
        if kind in {"bar", "grouped_bar", "line", "scatter"}:
            x_col = mapping.get("x")
            y_cols = mapping.get("y")
            if not isinstance(x_col, str) or not isinstance(y_cols, list) or not y_cols:
                fail(f"panels[{i}].data 需要 x 和 y 数组")
            for col in [x_col, *y_cols]:
                if col not in rows[0]:
                    fail(f"panels[{i}].data_file 缺少列：{col}")
            if kind in {"bar", "grouped_bar"}:
                categories = [str(row[x_col]) for row in rows]
                panel["categories"] = categories
                panel["series"] = [
                    {
                        "name": str(col),
                        "values": [as_number(row[col], f"panels[{i}].{col}") for row in rows],
                    }
                    for col in y_cols
                ]
            else:
                x_values = [as_number(row[x_col], f"panels[{i}].{x_col}") for row in rows]
                panel["series"] = [
                    {
                        "name": str(col),
                        "x": x_values,
                        "y": [as_number(row[col], f"panels[{i}].{col}") for row in rows],
                    }
                    for col in y_cols
                ]
        elif kind in {"box", "violin"}:
            group_col = mapping.get("group")
            value_col = mapping.get("value")
            if not isinstance(group_col, str) or not isinstance(value_col, str):
                fail(f"panels[{i}].data 需要 group 和 value 列")
            if group_col not in rows[0] or value_col not in rows[0]:
                fail(f"panels[{i}].data_file 缺少列：{group_col}/{value_col}")
            groups: dict[str, list[float]] = {}
            for row in rows:
                groups.setdefault(str(row[group_col]), []).append(
                    as_number(row[value_col], f"panels[{i}].{value_col}")
                )
            panel["groups"] = [{"name": k, "values": v} for k, v in groups.items()]
        else:
            fail(f"panels[{i}].data_file 不支持图表类型：{kind}")
        # The generated spec/render.py must be self-contained: keep the
        # expanded arrays, drop the now-unneeded file reference.
        panel.pop("data_file", None)
        panel.pop("data", None)


def check_outputs(output_dir: Path, warnings: list[str]) -> list[str]:
    """Best-effort deterministic QA using optional system tools."""
    lines = ["# Figure QA", ""]
    pdf = output_dir / "figure.pdf"
    svg = output_dir / "figure.svg"
    png = output_dir / "preview.png"
    lines.append(f"- PDF: {'yes' if pdf.is_file() else 'missing'}")
    lines.append(f"- SVG: {'yes' if svg.is_file() else 'missing'}")
    lines.append(f"- PNG preview: {'yes' if png.is_file() else 'missing'}")
    if svg.is_file():
        text_count = len(re.findall(r"<text\b", svg.read_text(encoding="utf-8", errors="ignore")))
        lines.append(f"- SVG text elements: {text_count}")
        if text_count == 0:
            warnings.append("SVG 中没有文本节点，文字可能被转成了路径")
    if pdf.is_file() and shutil.which("pdffonts"):
        result = subprocess.run(
            ["pdffonts", str(pdf)], stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, timeout=30
        )
        if result.returncode == 0:
            font_lines = [line for line in result.stdout.splitlines()[2:] if line.strip()]
            lines.append(f"- PDF fonts: {len(font_lines)}")
            not_embedded = [line for line in font_lines if " no " in line]
            if not_embedded:
                warnings.append(f"{len(not_embedded)} 个 PDF 字体未嵌入")
                lines.extend(f"  - 未嵌入：{line.split()[0]}" for line in not_embedded)
            if any("Type 3" in line for line in font_lines):
                warnings.append("PDF 包含 Type 3 字体，部分编辑/投稿流程不兼容")
    if warnings:
        lines.extend(["", "## Warnings", ""] + [f"- {warning}" for warning in warnings])
    else:
        lines.extend(["", "检查通过。"])
    (output_dir / "CHECK.md").write_text("\n".join(lines).rstrip() + "\n", encoding="utf-8")
    return lines


def validate_spec(spec: dict[str, Any]) -> None:
    output_dir = spec.get("output_dir")
    if not isinstance(output_dir, str) or not output_dir:
        fail("output_dir 必须是绝对路径")
    if not Path(output_dir).is_absolute():
        fail("output_dir 必须是绝对路径（避免相对路径落到不可预期的位置）")

    figure = spec.get("figure", {})
    if not isinstance(figure, dict):
        fail("figure 必须是对象")
    width = require_number(figure.get("width_in", 3.5), "figure.width_in")
    height = require_number(figure.get("height_in", 2.4), "figure.height_in")
    font_size = require_number(figure.get("font_size", 8), "figure.font_size")
    dpi = require_number(figure.get("dpi", 600), "figure.dpi")
    if not WIDTH_RANGE[0] <= width <= WIDTH_RANGE[1]:
        fail(f"figure.width_in 必须在 {WIDTH_RANGE} 内")
    if not HEIGHT_RANGE[0] <= height <= HEIGHT_RANGE[1]:
        fail(f"figure.height_in 必须在 {HEIGHT_RANGE} 内")
    if not FONT_RANGE[0] <= font_size <= FONT_RANGE[1]:
        fail(f"figure.font_size 必须在 {FONT_RANGE} 内")
    if not DPI_RANGE[0] <= dpi <= DPI_RANGE[1]:
        fail(f"figure.dpi 必须在 {DPI_RANGE} 内")

    panels = spec.get("panels")
    if not isinstance(panels, list) or not panels:
        fail("panels 必须是非空数组")
    if len(panels) > MAX_PANELS:
        fail(f"panels 最多 {MAX_PANELS} 个")
    for i, panel in enumerate(panels):
        if not isinstance(panel, dict):
            fail(f"panels[{i}] 必须是对象")
        kind = panel.get("kind")
        if kind not in {"bar", "grouped_bar", "line", "scatter", "box", "violin", "heatmap"}:
            fail(f"panels[{i}].kind 不支持：{kind!r}")
        if kind in {"bar", "grouped_bar"}:
            string_list(panel.get("categories"), f"panels[{i}].categories")
            series = panel.get("series")
            if not isinstance(series, list) or not series:
                fail(f"panels[{i}].series 必须是非空数组")
            for j, item in enumerate(series):
                values = number_list(item.get("values"), f"panels[{i}].series[{j}].values")
                if len(values) != len(panel["categories"]):
                    fail(f"panels[{i}].series[{j}].values 长度必须等于 categories")
        elif kind in {"line", "scatter"}:
            series = panel.get("series")
            if not isinstance(series, list) or not series:
                fail(f"panels[{i}].series 必须是非空数组")
            for j, item in enumerate(series):
                y = number_list(item.get("y"), f"panels[{i}].series[{j}].y")
                x = item.get("x")
                if x is not None:
                    x = number_list(x, f"panels[{i}].series[{j}].x")
                    if len(x) != len(y):
                        fail(f"panels[{i}].series[{j}].x 与 y 长度必须一致")
                if item.get("yerr") is not None:
                    err = number_list(item["yerr"], f"panels[{i}].series[{j}].yerr")
                    if len(err) != len(y):
                        fail(f"panels[{i}].series[{j}].yerr 与 y 长度必须一致")
        elif kind in {"box", "violin"}:
            groups = panel.get("groups")
            if not isinstance(groups, list) or not groups:
                fail(f"panels[{i}].groups 必须是非空数组")
            for j, item in enumerate(groups):
                number_list(item.get("values"), f"panels[{i}].groups[{j}].values")
        elif kind == "heatmap":
            matrix = panel.get("matrix")
            if not isinstance(matrix, list) or not matrix:
                fail(f"panels[{i}].matrix 必须是非空二维数组")
            width0 = None
            for r, row in enumerate(matrix):
                if not isinstance(row, list) or not row:
                    fail(f"panels[{i}].matrix[{r}] 必须是非空数组")
                number_list(row, f"panels[{i}].matrix[{r}]")
                if width0 is None:
                    width0 = len(row)
                elif len(row) != width0:
                    fail(f"panels[{i}].matrix 每行长度必须一致")
            xlabels = panel.get("xlabels")
            ylabels = panel.get("ylabels")
            if xlabels is not None:
                string_list(xlabels, f"panels[{i}].xlabels")
                if len(xlabels) != width0:
                    fail(f"panels[{i}].xlabels 长度必须等于矩阵列数")
            if ylabels is not None:
                string_list(ylabels, f"panels[{i}].ylabels")
                if len(ylabels) != len(matrix):
                    fail(f"panels[{i}].ylabels 长度必须等于矩阵行数")


def apply_style(figure_spec: dict[str, Any], warnings: list[str]) -> None:
    import matplotlib
    matplotlib.use("Agg")
    import matplotlib.pyplot as plt

    requested = str(figure_spec.get("font_family", "DejaVu Sans"))
    try:
        from matplotlib import font_manager
        font_manager.findfont(requested, fallback_to_default=False)
        family = requested
    except Exception:
        family = "DejaVu Sans"
        warn(warnings, f"字体 {requested!r} 不可用，已回退到 DejaVu Sans")

    font_size = float(figure_spec.get("font_size", 8))
    plt.rcParams.update({
        "font.family": family,
        "font.size": font_size,
        "axes.titlesize": font_size + 1,
        "axes.labelsize": font_size,
        "xtick.labelsize": max(5.5, font_size - 1),
        "ytick.labelsize": max(5.5, font_size - 1),
        "legend.fontsize": max(5.5, font_size - 1),
        "pdf.fonttype": 42,
        "ps.fonttype": 42,
        "svg.fonttype": "none",
        "axes.spines.top": False,
        "axes.spines.right": False,
        "axes.linewidth": 0.7,
        "grid.linewidth": 0.4,
        "grid.alpha": 0.25,
        "legend.frameon": False,
        "figure.facecolor": "white",
        "savefig.facecolor": "white",
        "savefig.bbox": "tight",
        "savefig.pad_inches": 0.02,
    })


def palette(panel: dict[str, Any]) -> list[str]:
    colors = panel.get("colors")
    if isinstance(colors, list) and colors and all(isinstance(c, str) for c in colors):
        return colors
    return DEFAULT_COLORS


def style_axis(ax: Any, panel: dict[str, Any], default_grid: bool = False) -> None:
    ax.set_title(
        (f"({panel['label']}) " if panel.get("label") else "") + str(panel.get("title", "")),
        loc="left",
        fontweight="bold",
    )
    if panel.get("xlabel"):
        ax.set_xlabel(str(panel["xlabel"]))
    if panel.get("ylabel"):
        ax.set_ylabel(str(panel["ylabel"]))
    grid = panel.get("grid", default_grid)
    if grid:
        ax.grid(True, axis="y", linestyle="--")
    else:
        ax.grid(False)
    ax.set_axisbelow(True)


def add_bar(ax: Any, panel: dict[str, Any], colors: list[str], grouped: bool) -> None:
    import numpy as np

    categories = [str(c) for c in panel["categories"]]
    series = panel["series"]
    x = np.arange(len(categories))
    count = max(1, len(series))
    width = 0.78 / count if grouped else 0.72
    for idx, item in enumerate(series):
        offset = (idx - (count - 1) / 2) * width if grouped else 0
        ax.bar(
            x + offset,
            [float(v) for v in item["values"]],
            width=width,
            label=str(item.get("name", f"Series {idx + 1}")),
            color=colors[idx % len(colors)],
            edgecolor="white",
            linewidth=0.4,
        )
    ax.set_xticks(x)
    ax.set_xticklabels(categories, rotation=float(panel.get("rotate_xticks", 0)))
    if len(series) > 1 or panel.get("legend", False):
        ax.legend(loc=panel.get("legend_loc", "upper right"))


def add_line(ax: Any, panel: dict[str, Any], colors: list[str]) -> None:
    for idx, item in enumerate(panel["series"]):
        y = [float(v) for v in item["y"]]
        x = [float(v) for v in item.get("x", range(len(y)))]
        label = str(item.get("name", f"Series {idx + 1}"))
        style = {
            "color": colors[idx % len(colors)],
            "marker": item.get("marker", MARKERS[idx % len(MARKERS)]),
            "linestyle": item.get("linestyle", LINESTYLES[idx % len(LINESTYLES)]),
            "linewidth": float(item.get("linewidth", 1.1)),
            "markersize": float(item.get("markersize", 3.2)),
            "label": label,
        }
        if item.get("yerr") is not None:
            ax.errorbar(x, y, yerr=[float(v) for v in item["yerr"]], capsize=2.2, **style)
        else:
            ax.plot(x, y, **style)
    if panel.get("legend", True):
        ax.legend(loc=panel.get("legend_loc", "best"))


def add_scatter(ax: Any, panel: dict[str, Any], colors: list[str]) -> None:
    for idx, item in enumerate(panel["series"]):
        y = [float(v) for v in item["y"]]
        x = [float(v) for v in item.get("x", range(len(y)))]
        size = float(item.get("size", 16))
        ax.scatter(
            x,
            y,
            s=size,
            color=colors[idx % len(colors)],
            marker=item.get("marker", MARKERS[idx % len(MARKERS)]),
            alpha=float(item.get("alpha", 0.85)),
            edgecolors="white",
            linewidths=0.3,
            label=str(item.get("name", f"Series {idx + 1}")),
        )
    if panel.get("legend", True):
        ax.legend(loc=panel.get("legend_loc", "best"))


def add_box(ax: Any, panel: dict[str, Any], colors: list[str], violin: bool) -> None:
    groups = panel["groups"]
    data = [[float(v) for v in g["values"]] for g in groups]
    names = [str(g.get("name", f"Group {i + 1}")) for i, g in enumerate(groups)]
    positions = list(range(1, len(data) + 1))
    if violin:
        parts = ax.violinplot(data, positions=positions, showmeans=False, showmedians=True, widths=0.75)
        for i, body in enumerate(parts["bodies"]):
            body.set_facecolor(colors[i % len(colors)])
            body.set_edgecolor("white")
            body.set_alpha(0.75)
        for key in ("cmedians", "cmaxes", "cmins", "cbars"):
            if key in parts:
                parts[key].set_color("#334155")
                parts[key].set_linewidth(0.7)
    else:
        bp = ax.boxplot(
            data,
            positions=positions,
            widths=0.55,
            patch_artist=True,
            showfliers=False,
            medianprops={"color": "#111827", "linewidth": 1.0},
            whiskerprops={"linewidth": 0.7},
            capprops={"linewidth": 0.7},
        )
        for i, patch in enumerate(bp["boxes"]):
            patch.set_facecolor(colors[i % len(colors)])
            patch.set_edgecolor("#334155")
            patch.set_linewidth(0.7)
    ax.set_xticks(positions)
    ax.set_xticklabels(names, rotation=float(panel.get("rotate_xticks", 0)))


def add_heatmap(ax: Any, panel: dict[str, Any], warnings: list[str]) -> None:
    import numpy as np

    matrix = np.asarray([[float(v) for v in row] for row in panel["matrix"]], dtype=float)
    image = ax.imshow(matrix, aspect="auto", cmap=str(panel.get("cmap", "viridis")))
    colorbar = ax.figure.colorbar(image, ax=ax, fraction=0.046, pad=0.03)
    if panel.get("colorbar_label"):
        colorbar.set_label(str(panel["colorbar_label"]))
    xlabels = panel.get("xlabels")
    ylabels = panel.get("ylabels")
    if xlabels:
        ax.set_xticks(range(len(xlabels)))
        ax.set_xticklabels([str(v) for v in xlabels], rotation=float(panel.get("rotate_xticks", 45)), ha="right")
    if ylabels:
        ax.set_yticks(range(len(ylabels)))
        ax.set_yticklabels([str(v) for v in ylabels])
    if panel.get("annotate"):
        if matrix.size > 144:
            warn(warnings, "heatmap 格子超过 144 个，已跳过硬编码数值标注")
        else:
            for r in range(matrix.shape[0]):
                for c in range(matrix.shape[1]):
                    ax.text(c, r, f"{matrix[r, c]:.2f}", ha="center", va="center", fontsize=5.5, color="white")


def add_annotations(ax: Any, panel: dict[str, Any]) -> None:
    annotations = panel.get("annotations")
    if not isinstance(annotations, list):
        return
    for item in annotations:
        if not isinstance(item, dict):
            continue
        x1 = require_number(item.get("x1"), "annotation.x1")
        x2 = require_number(item.get("x2"), "annotation.x2")
        y = require_number(item.get("y"), "annotation.y")
        h = float(item.get("h", 0.03))
        ax.plot([x1, x1, x2, x2], [y, y + h, y + h, y], color="#111827", linewidth=0.7, clip_on=False)
        ax.text((x1 + x2) / 2, y + h, str(item.get("text", "*")), ha="center", va="bottom", fontsize=7)


def render(spec: dict[str, Any], warnings: list[str]) -> Path:
    import os
    import tempfile
    # In sandboxed hosts the real home config may be read-only; keep
    # matplotlib's cache in a writable temp directory.
    os.environ.setdefault(
        "MPLCONFIGDIR", str(Path(tempfile.gettempdir()) / "siku-matplotlib-cache")
    )
    import matplotlib
    matplotlib.use("Agg")
    import matplotlib.pyplot as plt

    figure_spec = spec.get("figure", {})
    apply_style(figure_spec, warnings)
    panels = spec["panels"]
    layout = spec.get("layout", {})
    cols = int(layout.get("cols", min(2, len(panels))))
    cols = max(1, min(cols, 4, len(panels)))
    rows = int(math.ceil(len(panels) / cols))
    fig, axes_array = plt.subplots(
        rows,
        cols,
        figsize=(float(figure_spec.get("width_in", 3.5)), float(figure_spec.get("height_in", 2.4))),
        squeeze=False,
        constrained_layout=True,
    )
    axes = axes_array.flatten()
    for index, panel in enumerate(panels):
        ax = axes[index]
        kind = panel["kind"]
        colors = palette(panel)
        if kind in {"bar", "grouped_bar"}:
            add_bar(ax, panel, colors, grouped=(kind == "grouped_bar" or len(panel["series"]) > 1))
            style_axis(ax, panel, default_grid=False)
        elif kind == "line":
            add_line(ax, panel, colors)
            style_axis(ax, panel, default_grid=True)
        elif kind == "scatter":
            add_scatter(ax, panel, colors)
            style_axis(ax, panel, default_grid=True)
        elif kind in {"box", "violin"}:
            add_box(ax, panel, colors, violin=(kind == "violin"))
            style_axis(ax, panel, default_grid=False)
        elif kind == "heatmap":
            add_heatmap(ax, panel, warnings)
            style_axis(ax, panel, default_grid=False)
        add_annotations(ax, panel)
        if panel.get("xlim") is not None:
            ax.set_xlim([float(v) for v in panel["xlim"]])
        if panel.get("ylim") is not None:
            ax.set_ylim([float(v) for v in panel["ylim"]])
    for ax in axes[len(panels):]:
        ax.set_visible(False)

    output_dir = Path(spec["output_dir"])
    output_dir.mkdir(parents=True, exist_ok=True)
    dpi = int(float(figure_spec.get("dpi", 600)))
    fig.savefig(output_dir / "figure.pdf", format="pdf")
    fig.savefig(output_dir / "figure.svg", format="svg")
    fig.savefig(output_dir / "preview.png", format="png", dpi=dpi)
    plt.close(fig)

    try:
        shutil.copyfile(Path(__file__).resolve(), output_dir / "render.py")
    except OSError as exc:
        warn(warnings, f"复制 render.py 失败：{exc}")
    (output_dir / "spec.json").write_text(json.dumps(spec, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    return output_dir


def write_readme(spec: dict[str, Any], output_dir: Path, warnings: list[str]) -> None:
    meta = spec.get("meta", {})
    figure = spec.get("figure", {})
    lines = [
        f"# {meta.get('title', 'Figure')}",
        "",
        f"- 数据来源：{meta.get('data_source', '未填写')}",
        f"- 目标：{meta.get('venue', '未填写')}",
        f"- 尺寸：{figure.get('width_in', 3.5)} × {figure.get('height_in', 2.4)} in",
        f"- 字号：{figure.get('font_size', 8)} pt",
        f"- 字体：{figure.get('font_family', 'DejaVu Sans')}",
        "",
        "## 文件",
        "",
        "- `figure.pdf`：投稿/嵌入用矢量图",
        "- `figure.svg`：可编辑矢量图",
        "- `preview.png`：预览位图，不是最终稿",
        "- `spec.json`：图表规格",
        "- `render.py`：生成脚本副本",
        "- `CHECK.md`：字体/矢量/文本 QA 结果",
        "",
        "## 复现",
        "",
        "```bash",
        "python render.py spec.json",
        "```",
        "",
        "需要 matplotlib；建议使用工作区虚拟环境。",
    ]
    if meta.get("notes"):
        lines.extend(["", "## 备注", "", str(meta["notes"])])
    if warnings:
        lines.extend(["", "## 生成警告", ""] + [f"- {w}" for w in warnings])
    (output_dir / "README.md").write_text("\n".join(lines).rstrip() + "\n", encoding="utf-8")


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description="生成论文级矢量图表")
    parser.add_argument("spec", nargs="?", help="JSON 规格文件")
    parser.add_argument("--schema", action="store_true", help="打印 JSON 规格契约")
    args = parser.parse_args(argv)
    if args.schema:
        print(json.dumps(SCHEMA, ensure_ascii=False, indent=2))
        return 0
    if not args.spec:
        parser.error("需要 spec 文件，或使用 --schema")
    spec_path = Path(args.spec)
    spec = load_spec(spec_path)
    materialize_data(spec, spec_path.resolve().parent)
    validate_spec(spec)
    try:
        warnings: list[str] = []
        output_dir = render(spec, warnings)
        check_outputs(output_dir, warnings)
        write_readme(spec, output_dir, warnings)
    except ImportError as exc:
        fail(
            "缺少 matplotlib。请在工作区创建虚拟环境并安装：\n"
            "  uv venv .siku/venv\n"
            "  uv pip install --python .siku/venv/bin/python matplotlib\n"
            f"原始错误：{exc}"
        )
    except Exception as exc:
        fail(f"生成失败：{exc}")
    print(json.dumps({
        "output_dir": str(output_dir),
        "files": ["figure.pdf", "figure.svg", "preview.png", "render.py", "spec.json", "README.md", "CHECK.md"],
        "warnings": warnings,
    }, ensure_ascii=False))
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
