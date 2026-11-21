# 贡献指南

感谢你愿意参与思库。提交前请先确认改动范围、复现步骤和验证结果。

## 开发环境

- Node.js >= 20
- pnpm >= 9
- Rust >= 1.78
- Windows / macOS / Linux

```bash
git clone https://github.com/seekpath-ai/siku.git
cd siku
pnpm install
pnpm tauri dev
```

## 提交前检查

前端：

```bash
pnpm typecheck
pnpm lint
```

后端：

```bash
cd src-tauri
cargo check
cargo test
cargo clippy
```

如果改动涉及 PDF 解析、引用定位、同步或远程接入，请在 PR 描述里写清楚：

- 复现步骤
- 修改前后的行为
- 已跑过的测试
- 仍存在的边界问题

## Pull Request

- 一个 PR 尽量只解决一件事
- 说明“为什么改”，并写清“改了什么”
- 新增行为请补测试；修 bug 请尽量补回归测试
- 不要提交 API Key、日志中的隐私内容、用户数据库或构建产物

## 代码风格

- TypeScript/React 遵循现有 ESLint 配置
- Rust 遵循 `cargo fmt` / `cargo clippy`
- 提交信息用中文、简短描述改动
- 不主动提交与当前任务无关的格式化改动

## 文档

- 工具变更同步更新 [docs/agent-tools.md](docs/agent-tools.md)
- 远程接入变更同步更新 [docs/remote-channels.md](docs/remote-channels.md)
- 用户可见的行为/限制变化同步更新 README

## 许可

提交贡献即表示你同意以本项目的 [Apache License 2.0 + Commons Clause](LICENSE) 授权你的贡献。
