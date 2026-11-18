---
name: paper-diagram
description: 生成论文投稿级概念图：模型结构、pipeline、流程图、模块关系图。输出可编辑的 SVG、TikZ 源文件、可选 PDF 和预览 PNG。适合顶会/期刊论文、组会和技术报告。当用户要求画架构图、流程图、pipeline、模块图或论文示意图时使用。
scenarios: [顶会论文, 期刊投稿, 模型架构图, Pipeline 图, 组会报告]
rules: [矢量输出, TikZ 源码, SVG 可编辑, 单栏/双栏尺寸, 显式布局]
tools: [bash, file_write, file_read]
---

# 论文示意图技能

用 `scripts/paper_diagram.py` 从 JSON 规格生成论文级示意图。不要自己从零写 SVG/TikZ 排版代码——尺寸、锚点、箭头、字体和导出规则已固化在脚本里。

## 运行环境（先读）

脚本只使用 Python 标准库；**不要求 LaTeX**。

- 有 `pdflatex`：自动编译 TikZ 并生成 `figure.pdf`；
- 没有 `pdflatex`：仍会生成 `figure.svg`、`diagram.tex` 和 `render.py`；
- 预览 PNG 依赖 `pdftoppm` 或 ImageMagick `convert`；都没有时只输出 SVG/TikZ。

不需要安装 Python 包。

## 工作流程

### 1. 确认图的结构

先确认：

- 图的用途：论文单栏 / 双栏 / 幻灯片；
- 模块和箭头关系；
- 是否需要分组（Encoder / Decoder / Training / Inference）；
- 尺寸：单栏约 3.5in，双栏约 7in。

**不要凭空编造实验模块或数据流程。** 用户没有给出结构时，先列出建议的节点和边，确认后再画。

### 2. 写 JSON 规格

查看契约：

```bash
$PY scripts/paper_diagram.py --schema
```

坐标单位是**英寸**，原点在**左上角**：

```json
{
  "output_dir": "/absolute/path/to/diagram",
  "meta": {
    "title": "Figure 2",
    "data_source": "用户描述 / 论文方法",
    "venue": "NeurIPS"
  },
  "canvas": {
    "width_in": 7.0,
    "height_in": 2.6,
    "font_size": 8
  },
  "layout": {
    "mode": "grid",
    "cols": 4,
    "margin_x": 0.25,
    "margin_y": 0.35,
    "gap_x": 0.25,
    "gap_y": 0.35
  },
  "groups": [
    { "id": "encoder", "label": "Encoder", "x": 0.2, "y": 0.3, "w": 3.0, "h": 1.9 }
  ],
  "nodes": [
    { "id": "input", "label": "Input", "x": 0.5, "y": 1.0, "w": 1.0, "h": 0.45, "shape": "round" },
    { "id": "enc", "label": "Transformer\nEncoder", "x": 1.9, "y": 1.0, "w": 1.0, "h": 0.6, "shape": "rect" }
  ],
  "edges": [
    { "from": "input", "to": "enc", "label": "", "arrow": "->" }
  ]
}
```

### 3. 自动布局

不想手算坐标时，用 `layout.mode = "grid"`：

```json
"layout": { "mode": "grid", "cols": 4, "margin_x": 0.25, "margin_y": 0.35, "gap_x": 0.25, "gap_y": 0.35 }
```

脚本会按 `nodes` 顺序排布；节点仍需给 `w`/`h`，`x`/`y` 会被忽略。分组仍是显式坐标。

### 4. 生成

```bash
$PY scripts/paper_diagram.py diagram-spec.json
```

输出：

```text
figure.svg      可编辑矢量图
diagram.tex     TikZ 源码
diagram.drawio  draw.io 可编辑文件
figure.pdf      有 pdflatex 时生成
preview.png     预览（有转换工具时）
render.py       可复现脚本副本
spec.json       规格
README.md       生成说明
CHECK.md        字体/矢量/文本 QA 结果
```

### 5. 视觉检查

先看 `CHECK.md`，确认 PDF 字体嵌入、SVG 保留文本、TikZ 源存在；再用 `read_media_file` 查看 `preview.png`，检查：

- 节点是否重叠；
- 箭头是否穿过文字；
- 分组标签是否遮挡；
- 单栏字号是否过小；
- 边是否被节点覆盖；
- 整体留白是否均匀。

发现问题 → 修改坐标/尺寸 → 重新生成，最多迭代 2 轮。

### 6. 交付

优先交付：

- `figure.pdf` 用于论文；
- `figure.svg` 用于 Illustrator / Inkscape 编辑；
- `diagram.tex` 用于 TikZ 继续编辑；
- `diagram.drawio` 用于 draw.io 继续编辑；
- `spec.json` + `render.py` 用于复现。

## 支持范围

MVP 支持：

- 节点：`rect`、`round`、`ellipse`、`diamond`、`none`（纯文字）；
- 边：直线、虚线、单/双箭头、标签；
- 分组：显式矩形分组；
- 显式坐标布局；
- 单栏/双栏尺寸；
- SVG + TikZ + 可选 PDF/PNG。

暂不支持：

- 自动布局/自动避让；
- 曲线箭头、复杂正交路由；
- 图标、图片嵌入；
- 自动从论文 PDF 提取结构。

复杂图建议用 draw.io / Figma / Illustrator 继续编辑。

## 注意事项

- 坐标是左上角原点；节点 `x/y` 是左上角，不是中心；
- 边默认连接节点中心到中心的边界锚点；
- 节点不要重叠；脚本不会自动避让；
- `figure.svg` 中的文字保持为文本，可在 Illustrator/Inkscape 编辑；
- TikZ 需要 LaTeX 才能编译 PDF；没有 LaTeX 时 SVG 仍然可用。
