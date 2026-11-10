---
name: pptx
description: 创建专业演示文稿（.pptx）。适合学术汇报、商务提案、讨论分享等场合。内置排版设计规范（字号、字体、行距、信息密度），产出可直接使用。当用户要求做 PPT、幻灯片、演示文稿时使用。
requires:
  binaries: [python3, soffice]
  pip: [python-pptx, pypdfium2]
tools: [bash, file_write, file_read]
---

# PPT 制作技能

用 `scripts/build_deck.py` 从 JSON 大纲生成 .pptx。**不要**自己用 python-pptx 从零写排版代码——设计规范已固化在脚本里，绕过它就放弃了质量保障。

## 工作流程

### 1. 确认场景与主题

先确定两个参数；用户没说就根据用途判断，拿不准就问一句：

- **场景**（决定字号体系，见 `references/design-tokens.md`）：
  - `presentation` 普通教室/会议室演讲（默认）
  - `hall` 大会场/礼堂
  - `online` 小屏幕/线上会议
  - `reading` 阅读型/打印型（不发演讲、供人自己看）
- **主题**（决定配色与版式，见 `references/themes.md`）：
  - `academic` 学术汇报（组会、答辩、会议报告）
  - `business` 商务场合（提案、路演、汇报）
  - `discussion` 讨论分享（头脑风暴、工作坊、内部交流）

### 2. 拟大纲并控制信息密度

- 一份演示 10–25 页为宜；超出时与用户确认是否真的需要这么多
- **每页要点 ≤6 条，单条尽量 ≤30 字**；超了就拆页，绝不靠缩字号塞下
- 每页只讲一个观点；标题写结论（「缓存命中率提升 40%」），不要写名词（「缓存」）
- 大纲先给用户看一眼（纯文本列表），确认后再生成，避免返工

### 3. 写 JSON 大纲文件

按 `scripts/build_deck.py --schema` 输出的契约写 `<工作目录>/deck.json`。支持的页面类型：`title`、`section`、`bullets`、`two_column`、`text`、`image`、`table`、`chart`、`big_number`、`closing`。

- 图片用绝对路径
- `table`：列 ≤6、行 ≤10，超出拆页
- `chart`：`bar`（柱状）/ `line`（折线）/ `pie`（饼图，仅单系列）；类目 ≤12、系列 ≤4；**必须**用 `caption` 注明单位与数据来源——没有来源的数据页不可信
- 数据提炼是你的工作：从用户提供的长表里归纳出适合图表的关键维度，不要把原始大表直接塞进页面

### 4. 生成

```bash
python3 scripts/build_deck.py deck.json
```

脚本会**校验并拒绝**违反设计规范的内容（字号低于场景下限、要点过多、字段缺失），报错信息会告诉你怎么改；修正后重跑。生成成功输出 .pptx 的绝对路径。

### 5. 视觉自检（有 LibreOffice 时必做）

```bash
python3 scripts/render_preview.py <产出.pptx>
```

它把每页渲染成 PNG（依赖 soffice + pypdfium2）。用 read_media_file **逐页查看**，检查：

- 文字是否溢出文本框/超出页面边缘
- 留白是否均衡（有没有内容挤在角落）
- 图片是否被拉伸变形、表格是否被裁切

发现问题 → 修改 deck.json → 重新生成 → 重新自检，最多迭代 2 轮。没有 soffice 时跳过视觉检查并告知用户「未做视觉自检，建议打开文件检查排版」。

### 6. 交付

把 .pptx 的完整路径告诉用户，附一句话说明（页数、主题、是否通过视觉自检）。

## 原则

- **内容归你，排版归脚本**。你负责观点、逻辑、文案精炼；字号、配色、版式一律交给脚本和主题，不要在 JSON 里传自定义颜色/字号（脚本会拒绝）
- 宁多一页，不挤一页
- 用户提供的素材（图片、数据、参考文献）优先用进对应页，不要自己编造数据
