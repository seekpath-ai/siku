---
name: paper-assets
description: 将论文图表插入 Word (.docx) 或 PowerPoint (.pptx)，自动生成图注、编号并保存新文件。适合把 paper-figure / paper-diagram 生成的 PDF、SVG、PNG 放进论文或汇报。当用户要求插入图片、添加图注、生成 Figure 编号或把图表放进 Word/PPT 时使用。
scenarios: [论文写作, 期刊投稿, 组会报告, 技术报告]
rules: [保留原文件, 自动编号, 图注居中, 不覆盖输入, 输出新文件]
tools: [bash, file_write, file_read]
---

# 论文图表插入技能

用 `scripts/paper_assets.py` 把图片插入 Word/PowerPoint，生成图注和编号。

## 运行环境（先读）

在工作区创建独立环境并安装：

```bash
uv venv .siku/venv
uv pip install --python .siku/venv/bin/python python-docx python-pptx  # Windows: .siku/venv/Scripts/python.exe
```

没有 `uv` 时使用 `python -m venv`。不要修改系统 Python。PDF/SVG 转 PNG 还需要 `pdftoppm` 或 ImageMagick `convert`。

## 工作流程

### 1. 确认目标文件

先确认：

- 目标：`.docx` 还是 `.pptx`；
- 图片：优先 `figure.png` / `preview.png`；也接受 `figure.pdf` / `figure.svg`（自动转换）；
- 图注文字；
- 是追加到末尾，还是插入到某个锚点段落之后；
- PPT 是新加一页，还是插入已有页。

**不要覆盖原文件。** 输出必须是新文件。

### 2. 插入

Word：

```bash
$PY scripts/paper_assets.py insert \
  --target paper.docx \
  --image figure.png \
  --caption "Overall architecture of our method." \
  --output paper-with-figure.docx \
  --label Figure \
  --width-in 3.5 \
  --after "3 Method"
```

PPT：

```bash
$PY scripts/paper_assets.py insert \
  --target slides.pptx \
  --image figure.png \
  --caption "Ablation results." \
  --output slides-with-figure.pptx \
  --label Figure
```

- `--after` 可选；不写则追加到文档末尾；
- PPT 默认新增一页；`--slide-index N` 插入已有页（0 开始）；
- `--number N` 可强制编号；不写则扫描现有图注，取最大编号 +1；
- `--label` 支持 `Figure` / `图`；
- `--width-in` 控制 Word 图片宽度；PPT 会自动按页面适配。

### 3. 检查

脚本输出：

```text
paper-with-figure.docx
INSERT.md
```

`INSERT.md` 记录编号、图片、图注、目标文件。打开输出文件确认：

- 图片清晰；
- 图注在图片下方；
- 编号连续；
- 没有压住正文/其他元素。

### 4. 编号说明

- Word：扫描段落中以 `Figure N:` / `Fig. N.` / `图 N：` 开头的图注；
- PPT：扫描所有文本框；
- 取最大编号 +1；
- 只处理图注，不自动更新正文中的 `Figure 1` 引用；
- 多张图请按顺序多次插入。

## 限制

- 不计算正文引用编号；
- 不自动排版浮动图片（Word 内联图片）；
- 不自动调整已有页面布局；
- PDF/SVG 转 PNG 需要系统工具有对应能力；
- 复杂模板（双栏、文本框锚定）建议插入后用 Word/PPT 手动微调。
