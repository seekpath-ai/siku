import { useState } from 'react';
import { Check, Loader2, StickyNote } from 'lucide-react';
import { notesCreate, vaultCreate, vaultImport, vaultList, vaultSetCurrent } from '@/lib/tauri';
import { pickDirectory } from '@/lib/pickDirectory';

/** Welcome note doubles as a beginner's guide — a narrative tour, not a
 *  feature dump: it walks the read → excerpt → AI-summarize → note loop that
 *  makes the app more than a Zotero + Obsidian + chatbot bundle. */
const WELCOME_NOTE = `# 欢迎使用思库

这是一篇可以随便改、随时删的欢迎笔记。花三分钟读完，你就知道思库该怎么用了。

## 思库是什么

不只是文献管理、笔记、AI 对话的简单拼合——这三件事在思库里是**打通**的：

- 📚 **图书馆**管文献（本地 PDF / Zotero 导入）
- 📝 **笔记**承接收获（Markdown、双链、加密字段）
- 🤖 **AI 智能体**在两者之间干活：读得懂你的文献，也写得了你的笔记

## 一条典型的工作流

**1. 导入文献**：向导里已导入的可跳过；平时把 PDF 拖进窗口即可，也可以从 Zotero 一键迁移整个文库。

**2. 阅读与划线**：打开文献，选中文字——「翻译」是即看即弃的临时翻译，「摘录」则存进右侧的**智思**面板，攒成这篇文献的要点集。

**3. 让 AI 帮你消化**：
- 阅读时点击桌面的**宠物球**，它会自动变成「文献阅读助手」——让它总结要点、解释图表、翻译摘要
- 在**笔记页面**点开宠物球，它是「笔记整理助手」——直接说「帮我整理这篇笔记」，它会读懂当前笔记并重排结构
- 在**对话页**新建智能体，干更大的活儿，比如：
  > 「将我的图书馆里『多智能体』集合的文献整理成一篇综述，保存到笔记」

  它会自己翻阅集合里的文献、交叉对比、写成综述并存进你的笔记库——文献、笔记、AI 在这一步闭环。

**4. 沉淀与输出**：笔记支持导出 PDF；表格、代码块、大纲俱全；重要内容可以右键「锁定」（首次设置全局锁密码，锁定的内容对 AI 也不可见）。

## 顺手试试这些

密码字段（选中文字右键「转为密码字段」，阅读时遮盖）：!pw[这是被遮盖的内容]

| 常用操作 | 方式 |
| -------- | ---- |
| 新建笔记 | Ctrl+N |
| 全局截图提问 | Ctrl+Shift+S，截图直接进对话 |
| 笔记大纲 | 阅读视图 ⋯ 菜单 |
| 多端同步 | 设置 → 同步（账号或局域网直连） |

有问题就点开宠物球问它——它读的文档比你想象的多。祝用得顺手！
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
      const base = dir.split(/[\\/]/).filter(Boolean).pop() || 'Obsidian 导入';
      // Vault names are unique (the app seeds a default "cognitive-archive"
      // vault, so an identically named Obsidian folder would clash) —
      // auto-suffix instead of failing the import.
      const taken = new Set((await vaultList().catch(() => [])).map((v) => v.name));
      let name = base;
      for (let i = 2; taken.has(name); i++) {
        name = `${base} (${i})`;
      }
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
          <img src="/brand-logos/obsidian.svg" alt="Obsidian" className="w-3.5 h-3.5" />
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
