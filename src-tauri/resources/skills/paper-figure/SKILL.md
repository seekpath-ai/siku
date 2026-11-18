---
name: paper-figure
description: 生成论文投稿级数据图表：支持折线、柱状、分组柱、散点、箱线、小提琴、热力图和多面板，输出可编辑的 PDF/SVG、预览 PNG 与可复现源码。适合顶会/期刊论文、组会和技术报告。当用户要求做论文图表、实验结果图、对比图、消融图、统计图或需要投稿级矢量图时使用。
scenarios: [顶会论文, 期刊投稿, 组会报告, 技术报告]
rules: [单栏/双栏尺寸, 字体嵌入, 色盲友好, 矢量输出, 保留源码]
tools: [bash, file_write, file_read]
---

# 论文图表技能

用 `scripts/paper_figure.py` 从 JSON 规格生成论文级图表。不要自己从零写 matplotlib 排版代码——尺寸、字体、配色和导出规则已固化在脚本里。

## 运行环境（先读）

本技能不附带依赖声明。运行前先用 `bash` 检查 Python，并在工作区创建独立环境：

```bash
uv venv .siku/venv
uv pip install --python .siku/venv/bin/python matplotlib  # Windows: .siku/venv/Scripts/python.exe
```

没有 `uv` 时使用 `python -m venv .siku/venv` 加对应 `pip`。下文 `$PY` 表示这个工作区解释器的绝对路径。不要修改系统 Python，也不要全局安装依赖。

## 工作流程

### 1. 确认数据与用途

必须先确认：

- 数据来源：用户提供的 CSV/JSON/表格，或用户明确给出的数值；
- 图表类型：折线 / 柱状 / 分组柱 / 散点 / 箱线 / 小提琴 / 热力图；
- 单栏（约 3.5in）还是双栏（约 7in）；
- 目标：论文投稿 / 组会 / 报告。

**禁止编造数据。** 用户没有提供数据时，先要求数据；不要用“看起来合理”的数值填充。

### 2. 写 JSON 规格

先看契约：

```bash
$PY scripts/paper_figure.py --schema
```

把规格写到工作区，例如 `figure-spec.json`。核心字段：

```json
{
  "output_dir": "/absolute/path/to/figure",
  "meta": {
    "title": "Figure 1",
    "data_source": "用户提供的 results.csv",
    "venue": "NeurIPS"
  },
  "figure": {
    "width_in": 3.5,
    "height_in": 2.4,
    "font_size": 8
  },
  "panels": [
    {
      "kind": "bar",
      "ylabel": "Accuracy (%)",
      "categories": ["A", "B"],
      "series": [{"name": "Ours", "values": [81.2, 84.6]}]
    }
  ]
}
```

- 单栏宽度用 `3.5`，双栏用 `7.0`；
- 字号 7–9pt；
- 一个 panel 一个子图；
- 多面板用 `panels` 数组，`layout` 可指定行列；
- 颜色默认使用色盲友好色板，也可在 panel 里给 `colors`。

也可以不把数值抄进 JSON，直接引用数据文件：

```json
{
  "kind": "bar",
  "ylabel": "Accuracy (%)",
  "data_file": "results.csv",
  "data": { "x": "method", "y": ["accuracy", "latency"] }
}
```

- `data_file` 相对于 spec 文件所在目录，或使用绝对路径；
- 支持 CSV / TSV / JSON（JSON 为对象数组）；
- bar/line/scatter 使用 `data.x` + `data.y` 列；
- box/violin 使用 `data.group` + `data.value`；
- 数据缺失或非数字会直接报错，不会静默跳过。

**优先使用 `data_file`**，避免模型手工转录数据。

### 3. 生成

```bash
$PY scripts/paper_figure.py figure-spec.json
```

输出目录会得到：

```text
figure.pdf      投稿/嵌入用矢量图
figure.svg      可在 Illustrator / Inkscape 编辑
preview.png     预览
render.py       可复现脚本副本
spec.json       图表规格
README.md       数据来源、尺寸、字体和复现命令
CHECK.md        字体嵌入、SVG 文本、预览等 QA 结果
```

### 4. 视觉检查

先看 `CHECK.md`，确认 PDF 字体已嵌入、SVG 保留文本、预览已生成；再用 `read_media_file` 查看 `preview.png`，检查：

- 文字是否重叠；
- 图例是否遮挡数据；
- 坐标轴标签是否完整；
- 单栏字号是否过小；
- 颜色在灰度下是否仍可区分；
- 多面板是否对齐。

发现问题 → 修改 spec → 重新生成，最多迭代 2 轮。

### 5. 质量检查

生成后查看 `CHECK.md`：

- PDF 字体是否嵌入；
- 是否出现 Type 3 字体；
- SVG 是否保留文本节点；
- 预览是否生成。

如果字体未嵌入或 SVG 没有文本，先修正再交付。

### 6. 交付

优先交付整个图表包，而不是只给 PNG：

- `figure.pdf` 用于论文/Word/PPT；
- `figure.svg` 用于继续编辑；
- `spec.json` + `render.py` 用于复现；
- `README.md` 说明数据来源。

如果用户只需要插入 Word/PPT，把 `figure.pdf` 或 `preview.png` 交给对应技能。

## 支持范围

MVP 支持：

- `bar`：柱状图；
- `grouped_bar`：分组柱状图；
- `line`：折线图，可带误差棒；
- `scatter`：散点图；
- `box`：箱线图；
- `violin`：小提琴图；
- `heatmap`：热力图。

暂不支持：3D 图、交互图、自动从 PDF 提取数据、复杂示意图。

## 注意事项

- 不执行用户提供的脚本；
- 热力图矩阵必须完整，不能有缺失值；
- 论文插图优先 PDF/SVG，不要把 PNG 当最终稿；
- SVG 中文字保持为文本（可编辑），但目标机器缺少字体时可能替换；
- PDF 字体嵌入；若期刊要求转曲，需在排版阶段另行处理。
