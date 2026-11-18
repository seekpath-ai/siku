#!/usr/bin/env python3
"""paper_diagram.py — 从 JSON 规格生成论文级示意图。

用法:
    python3 paper_diagram.py --schema
    python3 paper_diagram.py diagram-spec.json

输出:
    figure.svg / diagram.tex
    figure.pdf（有 pdflatex 时）
    preview.png（有 pdftoppm/convert 时）
    render.py / spec.json / README.md

只依赖 Python 标准库；PDF/PNG 转换是可选能力。
"""

from __future__ import annotations

import argparse
import json
import math
import re
import shutil
import subprocess
import sys
from pathlib import Path
from typing import Any
from xml.sax.saxutils import escape as xml_escape

SCHEMA: dict[str, Any] = {
    "output_dir": "绝对路径；输出图表包写到该目录",
    "meta": {
        "title": "可选：图表标题",
        "data_source": "可选但强烈建议：结构来源",
        "venue": "可选：目标会议/期刊",
        "notes": "可选：补充说明",
    },
    "canvas": {
        "width_in": "画布宽度，单栏 3.5 / 双栏 7.0",
        "height_in": "画布高度",
        "font_size": "默认 8（pt）",
        "font_family": "可选，默认 DejaVu Sans",
        "background": "可选，默认 #FFFFFF",
    },
    "layout": {
        "mode": "manual | grid；grid 时按 nodes 顺序自动排布",
        "cols": "grid 列数，默认 min(3, nodes 数)",
        "margin_x": "grid 左边距（英寸），默认 0.3",
        "margin_y": "grid 上边距（英寸），默认 0.3",
        "gap_x": "列间距（英寸），默认 0.3",
        "gap_y": "行间距（英寸），默认 0.3",
    },
    "groups": [
        {
            "id": "分组 id",
            "label": "分组名",
            "x": "左上角 x（英寸）",
            "y": "左上角 y（英寸）",
            "w": "宽（英寸）",
            "h": "高（英寸）",
            "fill": "可选填充色，默认 #F1F5F9",
            "stroke": "可选边框色，默认 #94A3B8",
            "nodes": "可选：若省略 x/y/w/h，由这些节点计算边界",
        }
    ],
    "nodes": [
        {
            "id": "唯一 id",
            "label": "节点文字（支持 \\n）",
            "x": "左上角 x（英寸）",
            "y": "左上角 y（英寸）",
            "w": "宽（英寸）",
            "h": "高（英寸）",
            "shape": "rect | round | ellipse | diamond | none",
            "fill": "可选填充色",
            "stroke": "可选边框色",
            "text_color": "可选文字色",
            "font_size": "可选，覆盖 canvas.font_size",
        }
    ],
    "edges": [
        {
            "from": "起点 node id",
            "to": "终点 node id",
            "label": "可选边标签",
            "arrow": "-> | <- | <-> | -",
            "style": "solid | dashed",
            "color": "可选颜色",
            "from_side": "可选 top|right|bottom|left",
            "to_side": "可选 top|right|bottom|left",
            "label_offset": "[dx,dy]（英寸），可选",
        }
    ],
}

SHAPES = {"rect", "round", "ellipse", "diamond", "none"}
SIDES = {"top", "right", "bottom", "left"}
ARROWS = {"->", "<-", "<->", "-"}
MAX_NODES = 80
MAX_EDGES = 160


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


def num(value: Any, name: str) -> float:
    if not isinstance(value, (int, float)) or isinstance(value, bool):
        fail(f"{name} 必须是数字")
    n = float(value)
    if not math.isfinite(n):
        fail(f"{name} 不能是 NaN/Infinity")
    return n


def color(value: Any, name: str, fallback: str) -> str:
    if value is None:
        return fallback
    if not isinstance(value, str):
        fail(f"{name} 必须是颜色字符串")
    s = value.strip()
    if len(s) == 7 and s.startswith("#") and all(c in "0123456789abcdefABCDEF" for c in s[1:]):
        return s.upper()
    fail(f"{name} 必须是 #RRGGBB 颜色")


def text(value: Any, name: str) -> str:
    if not isinstance(value, str) or not value.strip():
        fail(f"{name} 必须是非空字符串")
    return value


def validate(spec: dict[str, Any], warnings: list[str]) -> None:
    output_dir = spec.get("output_dir")
    if not isinstance(output_dir, str) or not Path(output_dir).is_absolute():
        fail("output_dir 必须是绝对路径")
    canvas = spec.get("canvas")
    if not isinstance(canvas, dict):
        fail("canvas 必须是对象")
    width = num(canvas.get("width_in", 3.5), "canvas.width_in")
    height = num(canvas.get("height_in", 2.4), "canvas.height_in")
    font = num(canvas.get("font_size", 8), "canvas.font_size")
    if not 1.0 <= width <= 20.0 or not 1.0 <= height <= 20.0:
        fail("canvas 宽高必须在 1–20 英寸内")
    if not 6.0 <= font <= 18.0:
        fail("canvas.font_size 必须在 6–18pt 内")
    color(canvas.get("background"), "canvas.background", "#FFFFFF")

    nodes = spec.get("nodes")
    if not isinstance(nodes, list) or not nodes:
        fail("nodes 必须是非空数组")
    if len(nodes) > MAX_NODES:
        fail(f"nodes 最多 {MAX_NODES} 个")
    ids: set[str] = set()
    node_map: dict[str, dict[str, Any]] = {}
    for i, node in enumerate(nodes):
        if not isinstance(node, dict):
            fail(f"nodes[{i}] 必须是对象")
        node_id = text(node.get("id"), f"nodes[{i}].id")
        if node_id in ids:
            fail(f"nodes id 重复：{node_id}")
        ids.add(node_id)
        node_map[node_id] = node
        text(node.get("label"), f"nodes[{i}].label")
        shape = node.get("shape", "round")
        if shape not in SHAPES:
            fail(f"nodes[{i}].shape 不支持：{shape!r}")
        x = num(node.get("x"), f"nodes[{i}].x")
        y = num(node.get("y"), f"nodes[{i}].y")
        w = num(node.get("w"), f"nodes[{i}].w")
        h = num(node.get("h"), f"nodes[{i}].h")
        if w <= 0 or h <= 0:
            fail(f"nodes[{i}] 的 w/h 必须大于 0")
        if x < -0.01 or y < -0.01 or x + w > width + 0.01 or y + h > height + 0.01:
            warn(warnings, f"nodes[{i}] 超出画布（可忽略，但建议调整坐标）")
        color(node.get("fill"), f"nodes[{i}].fill", "#F8FAFC")
        color(node.get("stroke"), f"nodes[{i}].stroke", "#334155")
        color(node.get("text_color"), f"nodes[{i}].text_color", "#111827")
        if node.get("font_size") is not None:
            fs = num(node["font_size"], f"nodes[{i}].font_size")
            if not 6 <= fs <= 18:
                fail(f"nodes[{i}].font_size 必须在 6–18pt 内")

    groups = spec.get("groups", [])
    if groups is not None and not isinstance(groups, list):
        fail("groups 必须是数组")
    group_ids: set[str] = set()
    for i, group in enumerate(groups or []):
        if not isinstance(group, dict):
            fail(f"groups[{i}] 必须是对象")
        gid = text(group.get("id"), f"groups[{i}].id")
        if gid in group_ids:
            fail(f"groups id 重复：{gid}")
        group_ids.add(gid)
        text(group.get("label"), f"groups[{i}].label")
        refs = group.get("nodes")
        if refs is not None:
            if not isinstance(refs, list) or not refs:
                fail(f"groups[{i}].nodes 必须是非空数组")
            for ref in refs:
                if ref not in node_map:
                    fail(f"groups[{i}].nodes 引用了未知节点：{ref}")
        else:
            for key in ("x", "y", "w", "h"):
                num(group.get(key), f"groups[{i}].{key}")
            if float(group["w"]) <= 0 or float(group["h"]) <= 0:
                fail(f"groups[{i}] 的 w/h 必须大于 0")
        color(group.get("fill"), f"groups[{i}].fill", "#F1F5F9")
        color(group.get("stroke"), f"groups[{i}].stroke", "#94A3B8")

    # Overlapping nodes are the most common layout mistake; warn early.
    for i in range(len(nodes)):
        for j in range(i + 1, len(nodes)):
            a, b = nodes[i], nodes[j]
            if a.get("shape") == "none" or b.get("shape") == "none":
                continue
            ax, ay, aw, ah = (float(a[k]) for k in ("x", "y", "w", "h"))
            bx, by, bw, bh = (float(b[k]) for k in ("x", "y", "w", "h"))
            if ax < bx + bw - 0.01 and bx < ax + aw - 0.01 and ay < by + bh - 0.01 and by < ay + ah - 0.01:
                warn(warnings, f"节点可能重叠：{a['id']} 与 {b['id']}")

    edges = spec.get("edges", [])
    if edges is not None and not isinstance(edges, list):
        fail("edges 必须是数组")
    if len(edges or []) > MAX_EDGES:
        fail(f"edges 最多 {MAX_EDGES} 条")
    for i, edge in enumerate(edges or []):
        if not isinstance(edge, dict):
            fail(f"edges[{i}] 必须是对象")
        src = text(edge.get("from"), f"edges[{i}].from")
        dst = text(edge.get("to"), f"edges[{i}].to")
        if src not in node_map or dst not in node_map:
            fail(f"edges[{i}] 引用了未知节点：{src} -> {dst}")
        arrow = edge.get("arrow", "->")
        if arrow not in ARROWS:
            fail(f"edges[{i}].arrow 不支持：{arrow!r}")
        style = edge.get("style", "solid")
        if style not in {"solid", "dashed"}:
            fail(f"edges[{i}].style 只能是 solid/dashed")
        for side_key in ("from_side", "to_side"):
            side = edge.get(side_key)
            if side is not None and side not in SIDES:
                fail(f"edges[{i}].{side_key} 只能是 top/right/bottom/left")
        if edge.get("label") is not None and not isinstance(edge["label"], str):
            fail(f"edges[{i}].label 必须是字符串")
        color(edge.get("color"), f"edges[{i}].color", "#475569")
        offset = edge.get("label_offset")
        if offset is not None:
            if not isinstance(offset, list) or len(offset) != 2:
                fail(f"edges[{i}].label_offset 必须是 [dx,dy]")
            num(offset[0], f"edges[{i}].label_offset[0]")
            num(offset[1], f"edges[{i}].label_offset[1]")


def materialize_layout(spec: dict[str, Any], warnings: list[str]) -> None:
    layout = spec.get("layout", {})
    if not isinstance(layout, dict):
        fail("layout 必须是对象")
    mode = layout.get("mode", "manual")
    if mode == "manual":
        return
    if mode != "grid":
        fail(f"不支持的 layout.mode：{mode!r}")
    nodes = spec.get("nodes", [])
    if not isinstance(nodes, list) or not nodes:
        fail("grid 布局需要 nodes")
    canvas = spec.get("canvas", {})
    W = float(canvas.get("width_in", 3.5))
    H = float(canvas.get("height_in", 2.4))
    cols = int(layout.get("cols", min(3, len(nodes))))
    if cols < 1 or cols > 8:
        fail("layout.cols 必须在 1–8 内")
    margin_x = float(layout.get("margin_x", 0.3))
    margin_y = float(layout.get("margin_y", 0.3))
    gap_x = float(layout.get("gap_x", 0.3))
    gap_y = float(layout.get("gap_y", 0.3))
    rows = math.ceil(len(nodes) / cols)
    cell_w = (W - 2 * margin_x - (cols - 1) * gap_x) / cols
    if cell_w <= 0:
        fail("画布宽度不足以容纳 grid 列和间距")
    for i, node in enumerate(nodes):
        w = num(node.get("w"), f"nodes[{i}].w")
        h = num(node.get("h"), f"nodes[{i}].h")
        if w <= 0 or h <= 0:
            fail(f"nodes[{i}] 的 w/h 必须大于 0")
        if "x" in node or "y" in node:
            warn(warnings, f"grid 布局忽略 nodes[{i}] 的 x/y")
    row_heights = [0.0] * rows
    for i, node in enumerate(nodes):
        row = i // cols
        row_heights[row] = max(row_heights[row], float(node["h"]))
    y = margin_y
    for row in range(rows):
        for col in range(cols):
            i = row * cols + col
            if i >= len(nodes):
                break
            node = nodes[i]
            w, h = float(node["w"]), float(node["h"])
            node["x"] = margin_x + col * (cell_w + gap_x) + (cell_w - w) / 2
            node["y"] = y + (row_heights[row] - h) / 2
        y += row_heights[row] + gap_y
    bottom = y - gap_y
    if bottom > H + 0.01:
        warn(warnings, f"grid 布局高度 {bottom:.2f}in 超出画布 {H:.2f}in")


def node_center(node: dict[str, Any]) -> tuple[float, float]:
    return (float(node["x"]) + float(node["w"]) / 2, float(node["y"]) + float(node["h"]) / 2)


def anchor(node: dict[str, Any], toward: dict[str, Any], side: str | None = None) -> tuple[float, float]:
    x, y = float(node["x"]), float(node["y"])
    w, h = float(node["w"]), float(node["h"])
    cx, cy = node_center(node)
    if side == "left":
        return (x, cy)
    if side == "right":
        return (x + w, cy)
    if side == "top":
        return (cx, y)
    if side == "bottom":
        return (cx, y + h)
    tx, ty = node_center(toward)
    dx, dy = tx - cx, ty - cy
    if dx == 0 and dy == 0:
        return (cx, cy)
    shape = node.get("shape", "round")
    if shape == "none":
        return (cx, cy)
    if shape == "ellipse":
        scale = 1.0 / math.sqrt((dx / (w / 2)) ** 2 + (dy / (h / 2)) ** 2)
        return (cx + dx * scale, cy + dy * scale)
    if shape == "diamond":
        scale = 1.0 / (abs(dx) / (w / 2) + abs(dy) / (h / 2))
        return (cx + dx * scale, cy + dy * scale)
    # Rect/rounded rect: hit the nearest vertical or horizontal edge.
    sx = (w / 2) / abs(dx) if dx else float("inf")
    sy = (h / 2) / abs(dy) if dy else float("inf")
    scale = min(sx, sy)
    return (cx + dx * scale, cy + dy * scale)


def group_bounds(group: dict[str, Any], nodes_by_id: dict[str, dict[str, Any]]) -> tuple[float, float, float, float]:
    if group.get("nodes"):
        refs = [nodes_by_id[n] for n in group["nodes"]]
        x0 = min(float(n["x"]) for n in refs) - 0.12
        y0 = min(float(n["y"]) for n in refs) - 0.35
        x1 = max(float(n["x"]) + float(n["w"]) for n in refs) + 0.12
        y1 = max(float(n["y"]) + float(n["h"]) for n in refs) + 0.12
        return (x0, y0, x1 - x0, y1 - y0)
    return (float(group["x"]), float(group["y"]), float(group["w"]), float(group["h"]))


def svg_text_lines(label: str, cx: float, cy: float, font: float, color: str, family: str) -> str:
    lines = label.split("\n")
    line_h = font * 1.25
    first = cy - (len(lines) - 1) * line_h / 2
    out = []
    for i, line in enumerate(lines):
        out.append(
            f'<text x="{cx:.3f}" y="{first + i * line_h:.3f}" text-anchor="middle" '
            f'dominant-baseline="middle" font-family="{xml_escape(family, {chr(34): "&quot;"})}" '
            f'font-size="{font:.2f}pt" fill="{color}">{xml_escape(line)}</text>'
        )
    return "".join(out)


def svg_arrow_marker(color_hex: str) -> str:
    marker_id = "arrow-" + color_hex.lstrip("#").lower()
    return (
        f'<marker id="{marker_id}" viewBox="0 0 10 10" refX="9" refY="5" '
        f'markerWidth="5" markerHeight="5" orient="auto-start-reverse">'
        f'<path d="M 0 0 L 10 5 L 0 10 z" fill="{color_hex}"/></marker>'
    )


def render_svg(spec: dict[str, Any], warnings: list[str]) -> str:
    canvas = spec["canvas"]
    W, H = float(canvas["width_in"]), float(canvas["height_in"])
    font = float(canvas.get("font_size", 8))
    family = str(canvas.get("font_family", "DejaVu Sans"))
    background = color(canvas.get("background"), "canvas.background", "#FFFFFF")
    nodes = spec["nodes"]
    node_map = {n["id"]: n for n in nodes}
    groups = spec.get("groups", []) or []
    edges = spec.get("edges", []) or []

    # points = inches * 72; viewBox keeps the coordinates simple.
    def p(v: float) -> float:
        return v * 72.0

    parts = [
        f'<svg xmlns="http://www.w3.org/2000/svg" width="{W}in" height="{H}in" '
        f'viewBox="0 0 {p(W):.3f} {p(H):.3f}" role="img">',
        "<defs>",
    ]
    seen_colors: set[str] = set()
    for edge in edges:
        c = color(edge.get("color"), f"edge {edge['from']}->{edge['to']} color", "#475569")
        if c not in seen_colors:
            seen_colors.add(c)
            parts.append(svg_arrow_marker(c))
    parts.append("</defs>")
    parts.append(f'<rect x="0" y="0" width="{p(W):.3f}" height="{p(H):.3f}" fill="{background}"/>')

    # Groups behind everything.
    for group in groups:
        gx, gy, gw, gh = group_bounds(group, node_map)
        gfill = color(group.get("fill"), f"group {group['id']} fill", "#F1F5F9")
        gstroke = color(group.get("stroke"), f"group {group['id']} stroke", "#94A3B8")
        parts.append(
            f'<rect x="{p(gx):.3f}" y="{p(gy):.3f}" width="{p(gw):.3f}" height="{p(gh):.3f}" '
            f'rx="{p(0.04):.3f}" fill="{gfill}" fill-opacity="0.55" stroke="{gstroke}" '
            f'stroke-width="0.8" stroke-dasharray="4 3"/>'
        )
        parts.append(
            f'<text x="{p(gx + 0.06):.3f}" y="{p(gy + 0.18):.3f}" font-family="{xml_escape(family, {chr(34): "&quot;"})}" '
            f'font-size="{max(6.5, font - 1):.2f}pt" fill="{gstroke}">{xml_escape(str(group["label"]))}</text>'
        )

    # Edges behind nodes.
    for edge in edges:
        src, dst = node_map[edge["from"]], node_map[edge["to"]]
        x1, y1 = anchor(src, dst, edge.get("from_side"))
        x2, y2 = anchor(dst, src, edge.get("to_side"))
        c = color(edge.get("color"), f"edge {edge['from']}->{edge['to']} color", "#475569")
        dash = ' stroke-dasharray="4 3"' if edge.get("style") == "dashed" else ""
        marker = ""
        if edge.get("arrow", "->") in {"->", "<->"}:
            marker += f' marker-end="url(#arrow-{c.lstrip("#").lower()})"'
        if edge.get("arrow") in {"<-", "<->"}:
            marker += f' marker-start="url(#arrow-{c.lstrip("#").lower()})"'
        parts.append(
            f'<line x1="{p(x1):.3f}" y1="{p(y1):.3f}" x2="{p(x2):.3f}" y2="{p(y2):.3f}" '
            f'stroke="{c}" stroke-width="1.1"{dash}{marker}/>'
        )
        label = edge.get("label")
        if label:
            off = edge.get("label_offset", [0.0, -0.10])
            lx = (x1 + x2) / 2 + float(off[0])
            ly = (y1 + y2) / 2 + float(off[1])
            parts.append(
                f'<text x="{p(lx):.3f}" y="{p(ly):.3f}" text-anchor="middle" '
                f'font-family="{xml_escape(family, {chr(34): "&quot;"})}" font-size="{max(6.5, font - 1):.2f}pt" '
                f'fill="{c}" paint-order="stroke" stroke="#FFFFFF" stroke-width="2.4">{xml_escape(str(label))}</text>'
            )

    # Nodes.
    for i, node in enumerate(nodes):
        x, y, w, h = (float(node[k]) for k in ("x", "y", "w", "h"))
        shape = node.get("shape", "round")
        fill = color(node.get("fill"), f"node {node['id']} fill", "#F8FAFC")
        stroke = color(node.get("stroke"), f"node {node['id']} stroke", "#334155")
        text_color = color(node.get("text_color"), f"node {node['id']} text", "#111827")
        fs = float(node.get("font_size", font))
        if shape != "none":
            if shape == "ellipse":
                parts.append(
                    f'<ellipse cx="{p(x + w / 2):.3f}" cy="{p(y + h / 2):.3f}" '
                    f'rx="{p(w / 2):.3f}" ry="{p(h / 2):.3f}" fill="{fill}" stroke="{stroke}" stroke-width="1.1"/>'
                )
            elif shape == "diamond":
                parts.append(
                    f'<polygon points="{p(x + w / 2):.3f},{p(y):.3f} {p(x + w):.3f},{p(y + h / 2):.3f} '
                    f'{p(x + w / 2):.3f},{p(y + h):.3f} {p(x):.3f},{p(y + h / 2):.3f}" '
                    f'fill="{fill}" stroke="{stroke}" stroke-width="1.1"/>'
                )
            else:
                rx = p(0.05) if shape == "round" else 0
                parts.append(
                    f'<rect x="{p(x):.3f}" y="{p(y):.3f}" width="{p(w):.3f}" height="{p(h):.3f}" '
                    f'rx="{rx:.3f}" fill="{fill}" stroke="{stroke}" stroke-width="1.1"/>'
                )
        parts.append(svg_text_lines(str(node["label"]), p(x + w / 2), p(y + h / 2), fs, text_color, family))

    parts.append("</svg>")
    return "\n".join(parts) + "\n"


def tex_escape(value: str) -> str:
    table = {
        "\\": r"\textbackslash{}",
        "&": r"\&",
        "%": r"\%",
        "$": r"\$",
        "#": r"\#",
        "_": r"\_",
        "{": r"\{",
        "}": r"\}",
        "~": r"\textasciitilde{}",
        "^": r"\textasciicircum{}",
    }
    return "".join(table.get(ch, ch) for ch in value)


def tex_multiline(value: str) -> str:
    return r"\\".join(tex_escape(line) for line in value.split("\n"))


def tikz_color_defs(spec: dict[str, Any]) -> tuple[str, dict[str, str]]:
    colors: dict[str, str] = {}
    def add(c: str) -> None:
        if c not in colors:
            colors[c] = f"siku{c.lstrip('#').lower()}"
    canvas = spec["canvas"]
    add(color(canvas.get("background"), "canvas.background", "#FFFFFF"))
    for group in spec.get("groups", []) or []:
        add(color(group.get("fill"), "group fill", "#F1F5F9"))
        add(color(group.get("stroke"), "group stroke", "#94A3B8"))
    for node in spec["nodes"]:
        add(color(node.get("fill"), "node fill", "#F8FAFC"))
        add(color(node.get("stroke"), "node stroke", "#334155"))
        add(color(node.get("text_color"), "node text", "#111827"))
    for edge in spec.get("edges", []) or []:
        add(color(edge.get("color"), "edge color", "#475569"))
    lines = [f"\\definecolor{{{name}}}{{HTML}}{{{c.lstrip('#')}}}" for c, name in colors.items()]
    return "\n".join(lines), colors


def render_tikz(spec: dict[str, Any], warnings: list[str]) -> str:
    canvas = spec["canvas"]
    W, H = float(canvas["width_in"]), float(canvas["height_in"])
    font = float(canvas.get("font_size", 8))
    family = str(canvas.get("font_family", "DejaVu Sans"))
    nodes = spec["nodes"]
    node_map = {n["id"]: n for n in nodes}
    groups = spec.get("groups", []) or []
    edges = spec.get("edges", []) or []
    defs, colors = tikz_color_defs(spec)
    font_cmd = r"\fontsize{" + f"{font:.2f}" + "pt}{" + f"{font * 1.2:.2f}" + r"pt}\selectfont"
    small_font = r"\fontsize{" + f"{max(6.5, font - 1):.2f}" + "pt}{" + f"{max(6.5, font - 1) * 1.2:.2f}" + r"pt}\selectfont"

    def tikz_x(x: float) -> str:
        return f"{x:.4f}"

    def tikz_y(y: float) -> str:
        return f"{H - y:.4f}"

    lines = [
        r"\documentclass[tikz,border=2pt]{standalone}",
        r"\usepackage[T1]{fontenc}",
        r"\usepackage{lmodern}",
        r"\usepackage{tikz}",
        r"\usetikzlibrary{arrows.meta,shapes.geometric,positioning,backgrounds}",
        defs,
        r"\begin{document}",
        r"\begin{tikzpicture}[x=1in,y=1in,font=\sffamily" + font_cmd + r"]",
        rf"\useasboundingbox (0,0) rectangle ({W:.4f},{H:.4f});",
        rf"\fill[{colors[color(canvas.get('background'), 'canvas.background', '#FFFFFF')]}] (0,0) rectangle ({W:.4f},{H:.4f});",
    ]

    # Groups.
    for group in groups:
        gx, gy, gw, gh = group_bounds(group, node_map)
        gfill = colors[color(group.get("fill"), "group fill", "#F1F5F9")]
        gstroke = colors[color(group.get("stroke"), "group stroke", "#94A3B8")]
        lines.append(
            rf"\fill[{gfill},fill opacity=0.55,rounded corners=2pt] ({tikz_x(gx)},{tikz_y(gy + gh)}) rectangle ({tikz_x(gx + gw)},{tikz_y(gy)});"
        )
        lines.append(
            rf"\draw[{gstroke},dashed,line width=0.6pt,rounded corners=2pt] ({tikz_x(gx)},{tikz_y(gy + gh)}) rectangle ({tikz_x(gx + gw)},{tikz_y(gy)});"
        )
        lines.append(
            rf"\node[anchor=north west,text={gstroke},font={small_font},inner sep=2pt] at ({tikz_x(gx + 0.05)},{tikz_y(gy + 0.02)}) {{{tex_escape(str(group['label']))}}};"
        )

    # Edges.
    for i, edge in enumerate(edges):
        src, dst = node_map[edge["from"]], node_map[edge["to"]]
        x1, y1 = anchor(src, dst, edge.get("from_side"))
        x2, y2 = anchor(dst, src, edge.get("to_side"))
        c = colors[color(edge.get("color"), "edge color", "#475569")]
        arrow = edge.get("arrow", "->")
        arrow_spec = {
            "->": r"-{Stealth[length=1.8mm]}",
            "<-": r"{Stealth[length=1.8mm]}-",
            "<->": r"{Stealth[length=1.8mm]}-{Stealth[length=1.8mm]}",
            "-": "-",
        }[arrow]
        dash = ",dashed" if edge.get("style") == "dashed" else ""
        label = str(edge.get("label", "") or "").strip()
        label_part = ""
        if label:
            off = edge.get("label_offset", [0.0, -0.10])
            lx = (x1 + x2) / 2 + float(off[0])
            ly = (y1 + y2) / 2 + float(off[1])
            label_part = rf" node[above,text={c},fill=white,inner sep=1pt,font={small_font}] {{{tex_escape(label)}}}"
        lines.append(
            rf"\draw[{c},line width=0.8pt,{arrow_spec}{dash}] ({tikz_x(x1)},{tikz_y(y1)}) --{label_part} ({tikz_x(x2)},{tikz_y(y2)});"
        )

    # Nodes.
    for i, node in enumerate(nodes):
        x, y, w, h = (float(node[k]) for k in ("x", "y", "w", "h"))
        shape = node.get("shape", "round")
        fill = colors[color(node.get("fill"), "node fill", "#F8FAFC")]
        stroke = colors[color(node.get("stroke"), "node stroke", "#334155")]
        text_color = colors[color(node.get("text_color"), "node text", "#111827")]
        fs = float(node.get("font_size", font))
        node_font = r"\fontsize{" + f"{fs:.2f}" + "pt}{" + f"{fs * 1.2:.2f}" + r"pt}\selectfont"
        options = [
            f"draw={stroke}",
            f"fill={fill}",
            f"text={text_color}",
            "align=center",
            "inner sep=2pt",
            f"minimum width={w:.4f}in",
            f"minimum height={h:.4f}in",
            f"font={node_font}",
        ]
        if shape == "round":
            options.append("rounded corners=3pt")
        elif shape == "ellipse":
            options.append("ellipse")
        elif shape == "diamond":
            options.append("diamond")
            options.append(f"aspect={max(0.2, w / max(h, 0.01)):.3f}")
        elif shape == "none":
            options = ["draw=none", "fill=none", f"text={text_color}", "align=center", f"font={node_font}"]
        lines.append(
            rf"\node[{','.join(options)}] (n{i}) at ({tikz_x(x + w / 2)},{tikz_y(y + h / 2)}) {{{tex_multiline(str(node['label']))}}};"
        )

    lines.extend([r"\end{tikzpicture}", r"\end{document}", ""])
    return "\n".join(lines)


def run_tex(output_dir: Path, tex_path: Path, warnings: list[str]) -> Path | None:
    exe = shutil.which("pdflatex")
    if not exe:
        warn(warnings, "未找到 pdflatex，未生成 figure.pdf；可安装 TeX 后重新运行 render.py")
        return None
    result = subprocess.run(
        [exe, "-interaction=nonstopmode", "-halt-on-error", "-no-shell-escape", tex_path.name],
        cwd=output_dir,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        timeout=120,
    )
    for suffix in (".aux", ".log", ".out"):
        try:
            (output_dir / (tex_path.stem + suffix)).unlink()
        except OSError:
            pass
    if result.returncode != 0:
        warn(warnings, "pdflatex 编译失败，保留 diagram.tex；详见控制台输出")
        sys.stderr.write(result.stdout[-2000:] + "\n")
        return None
    pdf = output_dir / (tex_path.stem + ".pdf")
    if not pdf.is_file():
        warn(warnings, "pdflatex 未生成 PDF")
        return None
    target = output_dir / "figure.pdf"
    try:
        shutil.move(str(pdf), target)
    except OSError as exc:
        warn(warnings, f"移动 PDF 失败：{exc}")
        return None
    return target


def make_preview(output_dir: Path, pdf: Path | None, svg: Path, warnings: list[str]) -> Path | None:
    preview = output_dir / "preview.png"
    if pdf is not None and shutil.which("pdftoppm"):
        result = subprocess.run(
            ["pdftoppm", "-png", "-r", "200", "-singlefile", str(pdf), str(preview.with_suffix(""))],
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, timeout=60,
        )
        if result.returncode == 0 and preview.is_file():
            return preview
    if shutil.which("convert"):
        result = subprocess.run(
            ["convert", "-density", "200", str(svg), str(preview)],
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, timeout=60,
        )
        if result.returncode == 0 and preview.is_file():
            return preview
    warn(warnings, "未生成 preview.png（需要 pdftoppm 或 ImageMagick convert）")
    return None


def render_drawio(spec: dict[str, Any]) -> str:
    canvas = spec["canvas"]
    W, H = float(canvas["width_in"]), float(canvas["height_in"])
    font = float(canvas.get("font_size", 8))
    family = str(canvas.get("font_family", "DejaVu Sans"))
    nodes = spec["nodes"]
    node_map = {n["id"]: n for n in nodes}
    groups = spec.get("groups", []) or []
    edges = spec.get("edges", []) or []
    px = 96.0

    def esc(value: str) -> str:
        escaped = xml_escape(value, {'"': "&quot;"})
        return escaped.replace("\n", "&#10;")

    def sid(prefix: str, index: int) -> str:
        return f"{prefix}{index}"

    node_cells: list[str] = []
    node_parent: dict[str, str] = {}
    group_cells: list[str] = []
    for gi, group in enumerate(groups):
        gx, gy, gw, gh = group_bounds(group, node_map)
        gid = sid("g", gi)
        for node in nodes:
            cx, cy = node_center(node)
            if gx <= cx <= gx + gw and gy <= cy <= gy + gh:
                node_parent.setdefault(node["id"], gid)
        gfill = color(group.get("fill"), "group fill", "#F1F5F9")
        gstroke = color(group.get("stroke"), "group stroke", "#94A3B8")
        style = (
            "rounded=1;whiteSpace=wrap;html=1;container=1;collapsible=0;"
            f"fillColor={gfill};strokeColor={gstroke};dashed=1;"
            "verticalAlign=top;align=left;spacingLeft=6;spacingTop=2;"
            f"fontSize={max(6.5, font - 1):.2f};fontFamily={esc(family)};"
        )
        group_cells.append(
            f'<mxCell id="{gid}" value="{esc(str(group["label"]))}" style="{style}" vertex="1" parent="1">'
            f'<mxGeometry x="{gx * px:.2f}" y="{gy * px:.2f}" width="{gw * px:.2f}" height="{gh * px:.2f}" as="geometry"/>'
            "</mxCell>"
        )

    for i, node in enumerate(nodes):
        nid = sid("n", i)
        parent = node_parent.get(node["id"], "1")
        x = float(node["x"]) * px
        y = float(node["y"]) * px
        w = float(node["w"]) * px
        h = float(node["h"]) * px
        if parent != "1":
            # Draw.io child coordinates are relative to the container.
            gi = int(parent[1:])
            gx, gy, _, _ = group_bounds(groups[gi], node_map)
            x -= gx * px
            y -= gy * px
        shape = node.get("shape", "round")
        fill = color(node.get("fill"), "node fill", "#F8FAFC")
        stroke = color(node.get("stroke"), "node stroke", "#334155")
        text_color = color(node.get("text_color"), "node text", "#111827")
        if shape == "none":
            style = (
                "text;html=1;strokeColor=none;fillColor=none;align=center;verticalAlign=middle;"
                f"fontColor={text_color};fontSize={float(node.get('font_size', font)):.2f};fontFamily={esc(family)};"
            )
        else:
            shape_style = {
                "rect": "rounded=0;",
                "round": "rounded=1;",
                "ellipse": "ellipse;",
                "diamond": "rhombus;",
            }[shape]
            style = (
                f"{shape_style}whiteSpace=wrap;html=1;align=center;verticalAlign=middle;"
                f"fillColor={fill};strokeColor={stroke};fontColor={text_color};"
                f"fontSize={float(node.get('font_size', font)):.2f};fontFamily={esc(family)};"
            )
        node_cells.append(
            f'<mxCell id="{nid}" value="{esc(str(node["label"]))}" style="{style}" vertex="1" parent="{parent}">'
            f'<mxGeometry x="{x:.2f}" y="{y:.2f}" width="{w:.2f}" height="{h:.2f}" as="geometry"/>'
            "</mxCell>"
        )

    edge_cells: list[str] = []
    side_pos = {"top": (0.5, 0.0), "right": (1.0, 0.5), "bottom": (0.5, 1.0), "left": (0.0, 0.5)}
    for i, edge in enumerate(edges):
        src, dst = node_map[edge["from"]], node_map[edge["to"]]
        c = color(edge.get("color"), "edge color", "#475569")
        arrow = edge.get("arrow", "->")
        start_arrow = "classic" if arrow in {"<-", "<->"} else "none"
        end_arrow = "classic" if arrow in {"->", "<->"} else "none"
        style = (
            "edgeStyle=none;html=1;rounded=0;"
            f"startArrow={start_arrow};endArrow={end_arrow};endFill=1;"
            f"strokeColor={c};fontSize={max(6.5, font - 1):.2f};fontFamily={esc(family)};"
        )
        if edge.get("style") == "dashed":
            style += "dashed=1;"
        for side_key, prefix in (("from_side", "exit"), ("to_side", "entry")):
            side = edge.get(side_key)
            if side:
                sx, sy = side_pos[side]
                style += f"{prefix}X={sx};{prefix}Y={sy};{prefix}Dx=0;{prefix}Dy=0;"
        edge_cells.append(
            f'<mxCell id="{sid("e", i)}" value="{esc(str(edge.get("label", "") or ""))}" style="{style}" edge="1" parent="1" source="{sid("n", nodes.index(src))}" target="{sid("n", nodes.index(dst))}">'
            '<mxGeometry relative="1" as="geometry"/>'
            "</mxCell>"
        )

    modified = "2026-01-01T00:00:00.000Z"
    return (
        '<mxfile host="app.diagrams.net" modified="' + modified + '" agent="paper-diagram" version="24.0.0">\n'
        '  <diagram id="paper-diagram" name="Page-1">\n'
        f'    <mxGraphModel dx="{W * px:.0f}" dy="{H * px:.0f}" grid="0" gridSize="10" guides="1" tooltips="1" connect="1" arrows="1" fold="1" page="1" pageScale="1" pageWidth="{W * px:.2f}" pageHeight="{H * px:.2f}" math="0" shadow="0">\n'
        '      <root>\n'
        '        <mxCell id="0"/>\n'
        '        <mxCell id="1" parent="0"/>\n'
        + "\n".join(group_cells + node_cells + edge_cells) + "\n"
        '      </root>\n'
        '    </mxGraphModel>\n'
        '  </diagram>\n'
        '</mxfile>\n'
    )


def check_outputs(output_dir: Path, warnings: list[str]) -> None:
    lines = ["# Diagram QA", ""]
    pdf = output_dir / "figure.pdf"
    svg = output_dir / "figure.svg"
    tex = output_dir / "diagram.tex"
    drawio = output_dir / "diagram.drawio"
    png = output_dir / "preview.png"
    lines.append(f"- PDF: {'yes' if pdf.is_file() else 'missing'}")
    lines.append(f"- SVG: {'yes' if svg.is_file() else 'missing'}")
    lines.append(f"- TikZ source: {'yes' if tex.is_file() else 'missing'}")
    lines.append(f"- draw.io source: {'yes' if drawio.is_file() else 'missing'}")
    lines.append(f"- PNG preview: {'yes' if png.is_file() else 'missing'}")
    if svg.is_file():
        count = len(re.findall(r"<text\b", svg.read_text(encoding="utf-8", errors="ignore")))
        lines.append(f"- SVG text elements: {count}")
        if count == 0:
            warnings.append("SVG 中没有文本节点，文字可能被转成了路径")
    if pdf.is_file() and shutil.which("pdffonts"):
        result = subprocess.run(
            ["pdffonts", str(pdf)], stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, timeout=30
        )
        if result.returncode == 0:
            fonts = [line for line in result.stdout.splitlines()[2:] if line.strip()]
            lines.append(f"- PDF fonts: {len(fonts)}")
            if any(" no " in line for line in fonts):
                warnings.append("PDF 中存在未嵌入字体")
            if any("Type 3" in line for line in fonts):
                warnings.append("PDF 包含 Type 3 字体")
    if warnings:
        lines.extend(["", "## Warnings", ""] + [f"- {warning}" for warning in warnings])
    else:
        lines.extend(["", "检查通过。"])
    (output_dir / "CHECK.md").write_text("\n".join(lines).rstrip() + "\n", encoding="utf-8")


def write_readme(spec: dict[str, Any], output_dir: Path, warnings: list[str]) -> None:
    meta = spec.get("meta", {})
    canvas = spec["canvas"]
    lines = [
        f"# {meta.get('title', 'Diagram')}",
        "",
        f"- 结构来源：{meta.get('data_source', '未填写')}",
        f"- 目标：{meta.get('venue', '未填写')}",
        f"- 画布：{canvas.get('width_in', 3.5)} × {canvas.get('height_in', 2.4)} in",
        f"- 字号：{canvas.get('font_size', 8)} pt",
        "",
        "## 文件",
        "",
        "- `figure.svg`：可编辑矢量图",
        "- `diagram.tex`：TikZ 源码",
        "- `diagram.drawio`：draw.io 可编辑文件",
        "- `figure.pdf`：有 LaTeX 时生成",
        "- `preview.png`：预览",
        "- `spec.json`：结构规格",
        "- `render.py`：生成脚本副本",
        "- `CHECK.md`：字体/矢量/文本 QA 结果",
        "",
        "## 复现",
        "",
        "```bash",
        "python render.py spec.json",
        "```",
    ]
    if meta.get("notes"):
        lines.extend(["", "## 备注", "", str(meta["notes"])])
    if warnings:
        lines.extend(["", "## 生成警告", ""] + [f"- {w}" for w in warnings])
    (output_dir / "README.md").write_text("\n".join(lines).rstrip() + "\n", encoding="utf-8")


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description="生成论文级示意图")
    parser.add_argument("spec", nargs="?", help="JSON 规格文件")
    parser.add_argument("--schema", action="store_true", help="打印 JSON 规格契约")
    args = parser.parse_args(argv)
    if args.schema:
        print(json.dumps(SCHEMA, ensure_ascii=False, indent=2))
        return 0
    if not args.spec:
        parser.error("需要 spec 文件，或使用 --schema")
    spec = load_spec(Path(args.spec))
    warnings: list[str] = []
    materialize_layout(spec, warnings)
    validate(spec, warnings)
    output_dir = Path(spec["output_dir"])
    output_dir.mkdir(parents=True, exist_ok=True)
    svg_text = render_svg(spec, warnings)
    svg_path = output_dir / "figure.svg"
    svg_path.write_text(svg_text, encoding="utf-8")
    tex_text = render_tikz(spec, warnings)
    tex_path = output_dir / "diagram.tex"
    tex_path.write_text(tex_text, encoding="utf-8")
    (output_dir / "diagram.drawio").write_text(render_drawio(spec), encoding="utf-8")
    pdf = run_tex(output_dir, tex_path, warnings)
    preview = make_preview(output_dir, pdf, svg_path, warnings)
    check_outputs(output_dir, warnings)
    try:
        shutil.copyfile(Path(__file__).resolve(), output_dir / "render.py")
    except OSError as exc:
        warn(warnings, f"复制 render.py 失败：{exc}")
    (output_dir / "spec.json").write_text(json.dumps(spec, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    write_readme(spec, output_dir, warnings)
    files = ["figure.svg", "diagram.tex", "diagram.drawio", "spec.json", "render.py", "README.md", "CHECK.md"]
    if pdf:
        files.append("figure.pdf")
    if preview:
        files.append("preview.png")
    print(json.dumps({"output_dir": str(output_dir), "files": files, "warnings": warnings}, ensure_ascii=False))
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
