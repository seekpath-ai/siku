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

CI 产物命名约束：productName 必须保持 ASCII（`siku`）。deb/rpm 包名规范不支持中文（会得到 `-1.5.0-1.x86_64.rpm` 空名产物）；Windows 的 NSIS/WiX 在 GitHub runner 非 UTF-8 代码页下也会因中文 productName 构建失败。中文桌面快捷方式后续用 NSIS 自定义模板实现，不走 productName。

平台资源打包：tauri 按目标平台自动合并 `tauri.<平台>.conf.json`（无需 --config，CI ubuntu 任务显式传了是双保险）。**新增需要打包的资源文件时三个平台配置文件都要检查**：windows/macos/linux 各自的 resources 是自包含的，漏配会在用户机器上报 "crsqlite not found" 这类启动错误（开发机上不会暴露，因为会命中 `src-tauri/ext/` 的编译机路径）。

Linux 窗口坑：桌宠气泡/详情卡这类透明覆盖窗调 `setIgnoreCursorEvents` 必须**在 `show()` 之后**。tao 的 Linux 实现处理该请求时 `window.window().unwrap()`，窗口 realize 前调用必 panic（整应用退出），先 show 后设置即可规避（参考 `src/components/pet/PetBubbleWindow.tsx`）。

## 验证命令

- 前端：`npx tsc --noEmit` + `npx eslint <改动文件>`
- 后端：`cd src-tauri && cargo check`

## 提交约定

- 提交信息用中文、简略
- 只在用户明确要求时提交
