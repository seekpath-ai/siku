> **状态：待实施 / 已确认方案 A（v2，已对照代码现状修正）**
> 已和用户确认按方案 A 实施：新增 `files` 表，把 PDF/Word/Excel 等文件作为 vault 内一级对象管理。
> v2 修正：vault_id 为 TEXT uuid；文件存储复用现有 blob 池而非自建目录；建表时即注册 CRR；拖入导入使用 Tauri 原生拖拽事件。

# Vault 文件统一管理方案

## 背景
不是所有文档都适合归入「图书馆」（论文库）。用户希望像 Obsidian 一样，在笔记文件列表中统一管理本地文件（PDF、Word、Excel、图片等），而不是把这些文件强行当作论文处理。

## 目标
- 在笔记列表（NoteList）中同时显示笔记、文件夹和普通文件。
- 文件可以按 vault 隔离，放在某个笔记文件夹下或根目录。
- 支持拖拽文件到列表导入、双击用系统默认程序打开、重命名、移动、删除。
- 文件身份稳定：改名/移动不影响 ID。

## 选定方案：方案 A — 新增 `files` 表

### 数据模型

在 `src-tauri/schema_init.sql` 中新增 `files` 表（注意：`vaults.id` 与 `notes.vault_id` 均为 TEXT uuid，默认 vault id 固定为 `00000000-0000-0000-0000-000000000001`）：

```sql
CREATE TABLE IF NOT EXISTS files (
    id TEXT PRIMARY KEY,                -- UUID，稳定主键
    vault_id TEXT NOT NULL REFERENCES vaults(id) ON DELETE CASCADE,
    parent_id TEXT REFERENCES notes(id) ON DELETE SET NULL, -- 所在文件夹（笔记文件夹），NULL = 根目录
    name TEXT NOT NULL,                 -- 显示文件名（含扩展名）
    blob_path TEXT NOT NULL,            -- blob 相对路径，如 blobs/<sha256>.<ext>
    size INTEGER,
    mime_type TEXT,
    sort_order INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_files_vault ON files(vault_id);
CREATE INDEX IF NOT EXISTS idx_files_parent ON files(parent_id);
```

设计要点：

- **文件存储复用现有 blob 池**（`src-tauri/src/file_store.rs`）：物理文件存为 `{app_data_dir}/blobs/{sha256}.{ext}`，`blob_path` 入库。与 papers/attachments 同一机制，天然获得内容去重、完整性校验，且**已接入 WebRTC 多端同步**（`sync/attachments.rs` 按 `blobs/%` 收集）。不引入自建 `files/` 目录，也不引入 `data_dir`（该设置目前只影响 memory/skills 目录，不是通用存储根）。
- blob 不可变语义正好契合需求：**重命名只改 `name`，不动磁盘**；外部同内容文件自动去重；不同内容产生新 blob。
- 不单独存 `content_hash`——blob 文件名即 sha256。
- `parent_id` 是新建表上的 FK（`notes.parent_id` 本身是无 FK 的纯 TEXT 列，新建表加约束无迁移负担）。文件夹删除时 file 落到根目录（SET NULL），需核对与 notes 删除文件夹时子节点的现有行为一致。

**迁移与同步约束（重要）**：本项目无 sqlx migrations 目录，schema 由 `schema_init.sql` + `db.rs init()` 内代码迁移组成，之后 `register_crr_tables()` 把核心表注册为 CR-SQLite CRR 表。`db.rs` 中有硬约束：**CRR 注册必须在所有 schema 迁移之后**，否则 trigger 列数不匹配导致 UPDATE 崩溃。因此：

- `files` 表 DDL 加在 `schema_init.sql`；
- 建表时即将 `files` 加入 `db.rs` 的 `CORE_SYNC_TABLES` 注册为 CRR 表——为后续多端同步铺路，避免日后补注册时处理存量数据迁移。

### Rust 后端

新增 `src-tauri/src/core/file_item_service.rs`：

- `list_files(db, vault_id)`：列出 vault 下所有文件。
- `import_file(db, app_data_dir, vault_id, parent_id, source_path)`：
  - 调 `file_store::copy_file_to_blob` 复制进 blob 池（自动算 sha256、去重）；
  - 生成 UUID，读取 size/mime（mime 推断复用/提取 `file_service.rs` 的私有 `mime_guess`），文件名净化复用 `commands/attachments.rs` 的 `sanitize_filename`；
  - 插入 `files` 记录，返回 `FileItem`。
- `move_file(db, id, parent_id, sort_order)`：修改 `parent_id`/`sort_order`。
- `rename_file(db, id, name)`：只改 `name`，磁盘不动。
- `delete_file(db, id)`：删除记录。**blob 暂不删**（可能被 papers/attachments 共享引用）；如需清理，后续做引用计数式 GC，与 papers 的 blob 清理策略统一。
- `resolve_file_path(app_data_dir, id)`：解析 blob 绝对路径，供 open 使用——参照 `commands/library.rs` 中 `paper_open_attachment` 的现成模式（解析路径 → `file_service::open_in_system`）。

模型结构 `FileItem` 加到 `src-tauri/src/core/models.rs`。

新增 `src-tauri/src/commands/files.rs`（模式参照 `commands/notes.rs`：`State<AppState>` 取 db/app_data_dir → 调 service → `Result<T, String>`）：

- `files_list(vault_id: String) -> Vec<FileItem>`
- `files_import(vault_id: String, source_path: String, parent_id: Option<String>) -> FileItem`
- `files_move(id: String, parent_id: Option<String>, sort_order: Option<i64>) -> ()`
- `files_rename(id: String, name: String) -> FileItem`
- `files_delete(id: String) -> ()`
- `files_open(id: String) -> ()`

在 `src-tauri/src/commands/mod.rs` 声明模块，在 `src-tauri/src/lib.rs` 的 `tauri::generate_handler!` 列表中注册命令（按领域分组注释的惯例）。

### 前端

在 `src/lib/tauri.ts` 新增 `FileItem` 类型和 API 函数（invoke 包装，camelCase 参数对象，参照现有 `notesCreate`/`notesMove` 风格）：`filesList`、`filesImport`、`filesMove`、`filesRename`、`filesDelete`、`filesOpen`。

改造 `src/components/notes/NoteList.tsx`（受控组件，数据与回调均由父组件传入）：

1. Props 增加 `files: FileItem[]` 和相关回调：`onFileImport`、`onFileMove`、`onFileRename`、`onFileDelete`、`onFileOpen`。
2. 构建树时把 `files` 合并进现有 `childrenMap`：
   - 文件不是 folder，没有子节点；
   - 排序沿用现有惯例：系统文件夹置顶 → 文件夹在前 → 其余按 `sort_order`/`updated_at`，文件插入同级排序。
3. `renderNode` 增加文件分支：
   - 图标按扩展名区分（PDF、Word、Excel、图片、通用文件）；
   - 单击选中，双击打开（`filesOpen`）；
   - 右键菜单扩展文件分支：打开、重命名、移动、删除（复用自研 `ContextMenu`，参照现有单选/批量 items 模式）。
4. 内部移动：复用 NoteList 现有自实现 pointer-based DnD（`handleNodeMouseDown` 那套），文件节点作为可拖、不可放（非文件夹）的节点接入，落点计算 parent_id + sort_order 后调 `onFileMove`。
5. **外部文件拖入导入：不能用 HTML5 drop**（NoteList 注释已明确 WebView2 下 HTML5 DnD 不可靠，这也是内部拖拽自实现的原因）。改用 Tauri 原生事件 `getCurrentWindow().onDragDropEvent`，拿到拖入文件的绝对路径数组，根据落点计算 `parent_id` 后逐个调 `filesImport`。

改造 `src/routes/notes.tsx`：

- 加载当前 vault 的文件列表（`filesList`），传给 `NoteList`；
- 实现文件相关回调，调用上述 API，操作后刷新列表（与 notes 的刷新方式保持一致）。

### 边界与后续

- 文件不会进入 `papers` / `library`，保持论文库的纯粹性。
- PDF 文件可以在右键菜单中额外提供「导入到图书馆」选项（后续迭代）。
- 多端同步：建表时已注册 CRR，表数据随现有同步机制走；blob 文件本身已被 `sync/attachments.rs` 的 `blobs/%` 收集逻辑覆盖，需在同步联调时验证。
- blob 删除策略：file 删除不删 blob，后续与 papers 统一做引用计数 GC。
- 暂不支持在笔记正文中用 `![[filename.pdf]]` 引用文件；可在文件管理落地后扩展。

## 未选方案回顾

| 方案 | 说明 | 未选原因 |
|---|---|---|
| 方案 B：把文件当成特殊笔记 | 扩展 `notes` 表加 `is_file` | 污染笔记表语义，空字段多，维护混乱 |
| 方案 C：文件系统为唯一真相源 | 像 Obsidian 一样以本地文件夹为唯一来源 | 改动太大，需重建 SQLite 中心架构，且会脱离现有 blob + CRR 同步体系 |

## 实施清单

1. `schema_init.sql`：新增 `files` 表 + 索引；`db.rs`：`files` 加入 `CORE_SYNC_TABLES`。
2. `core/models.rs`：`FileItem` 结构；`core/file_item_service.rs`：五个 service 函数。
3. `commands/files.rs`：六个命令；`commands/mod.rs`、`lib.rs` 注册。
4. `src/lib/tauri.ts`：`FileItem` 类型 + 六个 API 包装。
5. `NoteList.tsx`：files 合并入树、文件渲染分支、右键菜单、内部 DnD 接入、Tauri 拖入导入。
6. `src/routes/notes.tsx`：加载文件列表 + 回调接线。
7. 验证：`cargo check` + `pnpm typecheck`；手动验证导入/打开/重命名/移动/删除/vault 切换隔离。

按本方案实施后，笔记列表将能同时管理笔记、文件夹和普通文件，体验接近 Obsidian 的 vault 文件树，且存储与同步完全复用现有 blob + CRR 体系。
