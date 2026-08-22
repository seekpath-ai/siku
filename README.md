# Siku（思库）

> AI 原生的本地优先知识工作台：从发现、消化到复用，一个人与知识的完整闭环

[![Tauri 2.0](https://img.shields.io/badge/Tauri-2.0-FFC131?logo=tauri)](https://v2.tauri.app)
[![Rust](https://img.shields.io/badge/Rust-1.78+-DEA584?logo=rust)](https://www.rust-lang.org)
[![React](https://img.shields.io/badge/React-19-61DAFB?logo=react)](https://react.dev)
[![TypeScript](https://img.shields.io/badge/TypeScript-5.6+-3178C6?logo=typescript)](https://www.typescriptlang.org)
[![pnpm](https://img.shields.io/badge/pnpm-9.0+-F69220?logo=pnpm)](https://pnpm.io)
[![Release](https://github.com/seekpath-ai/siku/actions/workflows/release.yml/badge.svg)](https://github.com/seekpath-ai/siku/actions/workflows/release.yml)

## 为什么做 Siku

如果你是科研工作者或深度阅读者，这些场景应该不陌生：

- **论文读不动** — 88 页英文论文，ChatGPT 只能贴碎片进去，它没"读过"全文，还把参考文献当正文
- **AI 说的不敢信** — 聊得头头是道，出处一个没有，幻觉和真知混在一起
- **工具链是断的** — 文献在 Zotero，笔记在 Obsidian，翻译在 DeepL，AI 在网页里，每个环节都靠复制粘贴搬运
- **收藏即坟场** — 好博客丢进收藏夹就吃灰；想整理？格式繁杂、废话连篇，半小时起步
- **不敢上云** — 未发表的想法、私人笔记，不想交给云端服务

Siku 把这些揉成一件事：**让 AI 在你本地的知识库里干活。**

## 一个闭环

**发现**（arXiv/CrossRef 定时巡检，博客丢链接）→ **消化**（PDF 阅读 + 划词翻译 + AI 整理）→ **固化**（双链笔记）→ **复用**（RAG 问答 / 知识图谱 / 写作取材）

全程 AI 搬运，数据全在你自己的硬盘上。

## 功能亮点

- 📖 **读得动** — 智能体直接读本地 PDF：自动分块、识别正文边界（跳过参考文献/附录）、控制单次输出预算，88 页论文 AI 是真的"读过"
- ✅ **信得过** — RAG 回答逐条带 chunk 引用，点击跳回 PDF 原文位置高亮，每句话都可验证
- 🔗 **长在一起** — 类 Zotero 的文献管理 + 类 Obsidian 的笔记（实时预览/阅读/源码三模式）长在同一个数据库里：笔记引用链回 PDF 页码，双向链接织成知识图谱
- ✨ **收藏 → 吸收** — 丢个博客链接，AI 抓正文、去噪、整理成结构化笔记，自动挂双链进知识库；整理过的知识才能被检索和复用
- 🤖 **不止科研** — ReAct 引擎 + 23+ 工具是底座，五大知识域（学术/学习/生活/阅读/个人）各自独立的 prompt 与知识库；定时任务、文件操作、跑脚本，生活工作任务一样能跑
- 🔒 **数据主权** — 全部数据在本地 SQLite，可配 Ollama 完全离线；多设备同步走 WebRTC P2P + 中继邮箱离线中转，端到端加密
- 🐾 **全局待命** — 桌面宠物悬浮球，不切窗口、随时唤起 AI

## 下载

从 [GitHub Releases](https://github.com/seekpath-ai/siku/releases/latest) 下载对应平台的安装包：

| 平台 | 安装包 |
|------|--------|
| Windows | `.msi` / `-setup.exe`（NSIS） |
| macOS | `.dmg`（Universal，Apple Silicon 与 Intel 通用） |
| Linux | `.AppImage` / `.deb` / `.rpm` |

应用内置自动更新：启动时自动检查新版本，确认后下载、验签、安装并重启。

> macOS 安装包未做苹果公证，首次打开如被 Gatekeeper 拦截，请在「系统设置 → 隐私与安全性」中手动允许。

## 技术栈

| 层级 | 技术 |
|------|------|
| 桌面框架 | Tauri 2.0 |
| 前端 | React 19 + TypeScript + Vite 6 |
| 样式 | Tailwind CSS + shadcn/ui |
| 状态管理 | Zustand + TanStack Query |
| 路由 | TanStack Router |
| 后端 | Rust |
| 数据库 | SQLite (WAL 模式) + sqlx |
| 搜索 | FTS5 + sqlite-vec + RRF 融合 |
| PDF | pdfium-render + lopdf |
| Embedding | fastembed-rs (BAAI/bge-small-zh-v1.5) |
| LLM | OpenAI / Anthropic / DeepSeek / SiliconFlow / Ollama / Qwen / Zhipu / Kimi / Gemini |
| Agent | ReAct 循环 + Tool Registry（23+ 工具 / Skill）+ SSE 流式 + 后台任务 + 定时任务 |

## 项目结构

```
├── src/                          # 前端源码 (React + Vite)
│   ├── components/
│   │   ├── layout/               # AppShell, Sidebar, TitleBar, TabBar
│   │   ├── chat/                 # 智能体对话
│   │   ├── library/              # 文献列表
│   │   ├── reader/               # PDF 阅读器 + 翻译覆盖层
│   │   ├── notes/                # 笔记编辑
│   │   ├── knowledge/            # 知识库
│   │   ├── research/             # 科研追踪
│   │   ├── settings/             # 设置面板
│   │   └── ui/                   # 共享 UI 原语
│   ├── routes/                   # TanStack Router 路由
│   ├── stores/                   # Zustand 状态
│   ├── hooks/                    # 自定义 Hooks
│   └── lib/                      # 工具函数 + Tauri IPC 封装
├── src-tauri/                    # Rust 后端
│   ├── src/
│   │   ├── main.rs               # 桌面入口
│   │   ├── lib.rs                # Tauri 配置 + 命令注册
│   │   ├── commands/             # Tauri IPC 命令处理
│   │   ├── core/                 # 数据库、模型、业务服务
│   │   ├── ai/                   # AI 模块
│   │   │   ├── agent/            # Agent 引擎 + 工具注册表
│   │   │   ├── llm/              # 多 Provider LLM 客户端
│   │   │   ├── rag/              # RAG 检索管线
│   │   │   ├── translation/      # 翻译服务 + 语义缓存
│   │   │   └── scraping/         # 学术抓取 (arXiv, CrossRef)
│   │   ├── pdf/                  # PDF 解析/渲染/分块
│   │   └── sync/                 # 多设备同步（WebRTC P2P + 中继邮箱）
│   ├── Cargo.toml
│   └── tauri.conf.json
├── docs/                         # 项目文档（含 agent-tools.md 工具清单）
├── public/                       # 静态资源
├── tests/                        # E2E 测试
├── package.json
├── vite.config.ts
└── index.html
```

## 快速开始

### 环境要求

- **Node.js** >= 20.0.0
- **pnpm** >= 9.0.0
- **Rust** >= 1.78
- **Windows** / **macOS** / **Linux**

### 安装与运行

```bash
# 克隆仓库
git clone https://github.com/seekpath-ai/siku.git
cd Zhiji

# 安装依赖
pnpm install

# 启动开发服务器 (仅前端)
pnpm dev

# 启动 Tauri 桌面应用 (前端 + 后端)
pnpm tauri dev

# 生产构建
pnpm tauri build
```

### 开发命令

```bash
pnpm dev          # Vite 开发服务器 (localhost:1420)
pnpm build        # TypeScript 编译 + Vite 构建
pnpm typecheck    # TypeScript 类型检查
pnpm lint         # ESLint 代码检查
pnpm tauri dev    # Tauri 开发模式
pnpm tauri build  # Tauri 生产打包
```

### Rust 命令

```bash
cd src-tauri
cargo check       # 编译检查
cargo test        # 运行测试
cargo clippy      # Lint 检查
```

## 发版

推送 `v*` 标签触发 GitHub Actions 三平台构建（Windows / macOS Universal / Linux），产物进入**草稿 Release**，检查无误后在网页上手动发布：

```bash
git tag v0.1.0
git push origin v0.1.0
```

发版前提：

- 仓库 Secret 配置 `TAURI_SIGNING_PRIVATE_KEY`（updater 签名私钥，由 `pnpm tauri signer generate` 生成；公钥已内置在 `tauri.conf.json`）
- 三处版本号保持一致：`src-tauri/tauri.conf.json`、`src-tauri/Cargo.toml`、`package.json`

发布后 `latest.json` 生效，旧版本客户端的自动更新即可检测到新版本。

## 架构

```
User → UI (React) → Tauri IPC → Commands → Services → SQLite / File Store
                        ↑                          ↑
                  Agent Engine (ReAct)       Tool Registry
                        ↓                          ↓
                  LLM Client (Multi-Provider)   23+ Tools / Skills
```

**Agent 信任模型**：只读工具自动放行；写/执行类工具按会话审批模式请求确认。文件工具限定在工作目录（项目沙箱，可配置全盘访问）。单次工具执行超时 320s（`bash` 自带最长 300s）。

**智能体内置工具**：见 [docs/agent-tools.md](docs/agent-tools.md)。

**流式通信**：后端 `Window::emit("agent:event", payload)` → 前端 `listen("agent:event", ...)`，事件类型包括 `thinking`、`tool_call`、`tool_approval_required`、`tool_result`、`delta`、`done`、`cancelled`、`ask_user`、`error`。

## 配置

### LLM Provider

支持以下 Provider，在应用设置中配置（可标记多模态/视觉模型）：

- OpenAI
- Anthropic (Claude)
- DeepSeek
- SiliconFlow
- Ollama (本地)
- Qwen（通义千问）
- Zhipu（智谱）
- Kimi（月之暗面）
- Gemini

代理为可选配置，留空直连。

### 日志

```bash
# 开启调试日志
SIKU_LOG=debug pnpm tauri dev
```

日志目录：`~/.siku/logs/`

## 许可

本项目采用 **Apache License 2.0 with Commons Clause** 授权。

- 允许自由使用、修改、分发以及嵌入到更大的产品中。
- **禁止**将本软件本身（或其功能构成主要价值的衍生产品）直接出售或作为商业 SaaS 提供。
- 详见根目录 [LICENSE](./LICENSE) 文件。

---

🤖 Built with [Tauri](https://tauri.app) + [React](https://react.dev) + [Rust](https://www.rust-lang.org)
