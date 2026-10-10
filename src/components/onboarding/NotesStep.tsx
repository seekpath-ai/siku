import { useState } from 'react';
import { Check, FolderOpen, Loader2, StickyNote } from 'lucide-react';
import { notesCreate, vaultCreate, vaultImport, vaultSetCurrent } from '@/lib/tauri';
import { pickDirectory } from '@/lib/pickDirectory';

/** Welcome note doubles as a living feature tour of the editor. */
const WELCOME_NOTE = `# 欢迎使用思库

这是一篇自动创建的欢迎笔记，顺手展示了编辑器的常用功能。可以随意改动或删除。

## 常用语法

**加粗**、*斜体*、~~删除线~~、\`行内代码\`，以及 - 列表、> 引用。

## 表格

| 功能 | 快捷键 |
| ---- | ------ |
| 新建笔记 | Ctrl+N |
| 全局截图 | Ctrl+Shift+S |

## 代码块

\`\`\`rust
fn main() {
    println!("让灵感涌动");
}
\`\`\`

## 密码字段

选中文字后右键可以转成掩码字段，阅读视图下默认遮盖：
!pw[在这里写下需要遮盖的内容]

## 小贴士

- 阅读视图 / 编辑视图随时切换，右上角大纲可快速跳转
- 笔记和文献一样支持多端同步（登录同步账号后自动进行）
- 文献阅读器里选中文字可以摘录到「智思」，还能划词临时翻译
`;

/** Onboarding step 4: seed the notes page — create the welcome note, or
 *  import an existing Obsidian vault (a folder of markdown files) as a new
 *  vault and switch to it. */
export function NotesStep() {
  const [noteBusy, setNoteBusy] = useState(false);
  const [noteDone, setNoteDone] = useState(false);
  const [noteError, setNoteError] = useState<string | null>(null);

  const [obsBusy, setObsBusy] = useState(false);
  const [obsResult, setObsResult] = useState<string | null>(null);
  const [obsError, setObsError] = useState<string | null>(null);

  const handleCreateWelcome = async () => {
    if (noteBusy || noteDone) return;
    setNoteBusy(true);
    setNoteError(null);
    try {
      await notesCreate('欢迎使用思库', WELCOME_NOTE);
      setNoteDone(true);
    } catch (e) {
      setNoteError(e instanceof Error ? e.message : String(e));
    } finally {
      setNoteBusy(false);
    }
  };

  const handleObsidianImport = async () => {
    if (obsBusy) return;
    const dir = await pickDirectory();
    if (!dir) return;
    setObsBusy(true);
    setObsResult(null);
    setObsError(null);
    try {
      const name = dir.split(/[\\/]/).filter(Boolean).pop() || 'Obsidian 导入';
      const vault = await vaultCreate(name);
      const result = await vaultImport(vault.id, dir);
      await vaultSetCurrent(vault.id);
      setObsResult(`已导入 ${result.files_imported} 个文件到「${name}」并切换为当前笔记库`);
    } catch (e) {
      setObsError(e instanceof Error ? e.message : String(e));
    } finally {
      setObsBusy(false);
    }
  };

  return (
    <div className="flex flex-col gap-3 text-left">
      {/* Welcome note */}
      <div className="px-4 py-3 rounded-xl bg-background border border-surface-hover">
        <div className="flex items-center gap-2 text-sm text-text-primary mb-1">
          <StickyNote size={14} className="text-primary" />
          创建欢迎笔记
        </div>
        <p className="text-[11px] text-text-secondary mb-2.5">
          内置常用语法示例（表格、代码块、密码字段等），兼作功能说明书，可随时删除。
        </p>
        <div className="flex items-center gap-2">
          <button
            onClick={handleCreateWelcome}
            disabled={noteBusy || noteDone}
            className="flex items-center gap-1.5 px-3 py-1.5 rounded-lg bg-primary text-white text-xs font-medium hover:opacity-90 disabled:opacity-40"
          >
            {noteBusy ? (
              <Loader2 size={12} className="animate-spin" />
            ) : noteDone ? (
              <Check size={12} />
            ) : null}
            {noteDone ? '已创建，在「笔记」页查看' : '创建欢迎笔记'}
          </button>
        </div>
        {noteError && <div className="mt-1.5 text-[11px] text-red-400 break-words">{noteError}</div>}
      </div>

      {/* Obsidian import */}
      <div className="px-4 py-3 rounded-xl bg-background border border-surface-hover">
        <div className="flex items-center gap-2 text-sm text-text-primary mb-1">
          <FolderOpen size={14} className="text-primary" />
          从 Obsidian 文件夹导入
        </div>
        <p className="text-[11px] text-text-secondary mb-2.5">
          选择一个 Obsidian 库文件夹（或任意 markdown 文件夹），导入为新的笔记库并切换过去。
        </p>
        <div className="flex items-center gap-2">
          <button
            onClick={handleObsidianImport}
            disabled={obsBusy}
            className="flex items-center gap-1.5 px-3 py-1.5 rounded-lg border border-surface-hover text-xs text-text-secondary hover:bg-surface-hover disabled:opacity-40"
          >
            {obsBusy && <Loader2 size={12} className="animate-spin" />}
            {obsBusy ? '导入中…' : '选择文件夹导入'}
          </button>
          {obsResult && <span className="text-[11px] text-text-secondary">{obsResult}</span>}
        </div>
        {obsError && <div className="mt-1.5 text-[11px] text-red-400 break-words">{obsError}</div>}
      </div>
    </div>
  );
}
