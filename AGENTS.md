# 思库（siku）开发须知

## 发版 checklist

安装包版本号不读取 git tag，来自以下三处硬编码，发版时必须同步修改：

1. `src-tauri/tauri.conf.json` 的 `"version"`（NSIS 安装包文件名、"关于"页）
2. `package.json` 的 `"version"`
3. `src-tauri/Cargo.toml` 的 `version`（改后跑一次 `cargo check` 刷新 `Cargo.lock`）

流程：

```bash
# 1. 修改上述三处版本号为 X.Y.Z
# 2. cargo check 刷新 Cargo.lock（cd src-tauri && cargo check）
# 3. 提交版本号变更
git add -A && git commit -m "发布 vX.Y.Z"
# 4. 打带更新简介的注释标签并推送
git tag -a vX.Y.Z -m "vX.Y.Z\n\n- 更新点1\n- 更新点2"
git push origin main && git push origin vX.Y.Z
# 5. 编译（此时产物文件名才是新版本号）
pnpm tauri build --target x86_64-pc-windows-gnu
```

顺序要点：**先改版本号再打 tag 最后编译**，保证 tag、仓库、二进制三者一致。

CI Linux 产物说明：deb/rpm 包名规范要求 ASCII，中文 productName 会被剥成空名（出现 `-1.5.0-1.x86_64.rpm` 这类文件），故 CI 的 ubuntu 任务用 `--config src-tauri/tauri.linux.conf.json` 把 productName 覆盖为 `siku`；该文件不含版本号，发版无需改动。

## 验证命令

- 前端：`npx tsc --noEmit` + `npx eslint <改动文件>`
- 后端：`cd src-tauri && cargo check`

## 提交约定

- 提交信息用中文、简略
- 只在用户明确要求时提交
