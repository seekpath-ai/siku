import { useEffect, useState } from 'react';
import { FolderPlus, FolderOpen, Loader2, X } from 'lucide-react';
import { gitAvailable } from '@/lib/tauri';

interface NewProjectDialogProps {
  open: boolean;
  onClose: () => void;
  /** Create the project (backend auto-creates the directory when missing).
   *  Returns the created project id, or null/throws on failure. */
  onCreate: (path: string, name?: string, gitInit?: boolean) => Promise<string | null>;
}

/** New-project dialog: name + directory path + optional git bootstrap. Unlike
 *  the old bare directory picker (existing folders only), the path may point
 *  at a folder that does not exist yet — the backend creates it ("新建项目
 *  目录"). The git checkbox greys out when no git binary is detected. */
export function NewProjectDialog({ open, onClose, onCreate }: NewProjectDialogProps) {
  const [name, setName] = useState('');
  const [path, setPath] = useState('');
  const [gitInit, setGitInit] = useState(false);
  const [gitOk, setGitOk] = useState<boolean | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Detect git availability when the dialog opens.
  useEffect(() => {
    if (!open) return;
    gitAvailable().then(setGitOk).catch(() => setGitOk(false));
  }, [open]);

  if (!open) return null;

  const handleBrowse = async () => {
    try {
      const { open: openDialog } = await import('@tauri-apps/plugin-dialog');
      const selected = await openDialog({ directory: true, multiple: false, title: '选择项目目录' });
      if (typeof selected === 'string') setPath(selected);
    } catch {
      // dialog cancelled or unavailable
    }
  };

  const handleSubmit = async () => {
    const p = path.trim();
    if (!p) {
      setError('请填写或选择项目目录');
      return;
    }
    setBusy(true);
    setError(null);
    try {
      const id = await onCreate(p, name.trim() || undefined, gitInit || undefined);
      if (id) {
        setName('');
        setPath('');
        setGitInit(false);
        onClose();
      } else {
        setError('创建失败，请查看日志');
      }
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="fixed inset-0 z-[200] flex items-center justify-center">
      <div className="absolute inset-0 bg-black/50 backdrop-blur-sm" onClick={onClose} />
      <div className="relative w-full max-w-md mx-4 bg-surface border border-surface-hover rounded-xl shadow-2xl overflow-hidden">
        <div className="flex items-center gap-3 px-4 py-3 border-b border-surface-hover">
          <FolderPlus size={18} className="text-primary" />
          <span className="text-sm font-medium text-text-primary">新建项目</span>
          <button
            onClick={onClose}
            className="ml-auto p-1 rounded text-text-secondary/60 hover:text-text-primary hover:bg-surface-hover transition-colors"
          >
            <X size={14} />
          </button>
        </div>

        <div className="px-4 py-4 space-y-3">
          <div>
            <label className="block text-xs text-text-secondary mb-1">项目名称（可选）</label>
            <input
              value={name}
              onChange={(e) => setName(e.target.value)}
              placeholder="默认取文件夹名"
              className="w-full h-8 px-2.5 rounded-lg bg-background border border-surface-hover text-xs text-text-primary outline-none focus:border-primary/50"
            />
          </div>
          <div>
            <label className="block text-xs text-text-secondary mb-1">项目目录</label>
            <div className="flex items-center gap-2">
              <input
                value={path}
                onChange={(e) => setPath(e.target.value)}
                placeholder="/path/to/project"
                className="flex-1 h-8 px-2.5 rounded-lg bg-background border border-surface-hover text-xs text-text-primary outline-none focus:border-primary/50"
              />
              <button
                onClick={handleBrowse}
                className="h-8 px-2.5 flex items-center gap-1 rounded-lg border border-surface-hover text-xs text-text-secondary hover:text-text-primary hover:bg-surface-hover transition-colors shrink-0"
              >
                <FolderOpen size={13} />
                浏览
              </button>
            </div>
            <p className="mt-1 text-[11px] text-text-secondary/60">
              目录不存在时会自动创建（可在「浏览」选中父目录后，在路径末尾补上新的文件夹名）。
            </p>
          </div>
          <label
            className={`flex items-center gap-2 text-xs ${
              gitOk === false ? 'text-text-secondary/40 cursor-not-allowed' : 'text-text-secondary cursor-pointer'
            }`}
            title={gitOk === false ? '未检测到 git，请先安装' : 'git init 并生成通用 .gitignore 模板'}
          >
            <input
              type="checkbox"
              checked={gitInit}
              disabled={gitOk === false}
              onChange={(e) => setGitInit(e.target.checked)}
              className="accent-primary"
            />
            初始化 git 仓库（git init + .gitignore）
            {gitOk === false && <span className="text-text-secondary/40">· 未检测到 git，请先安装</span>}
          </label>
          {error && <div className="text-xs text-red-400">{error}</div>}
        </div>

        <div className="flex justify-end gap-2 px-4 py-3 border-t border-surface-hover">
          <button
            onClick={onClose}
            className="px-3 py-1.5 rounded-lg text-xs text-text-secondary hover:text-text-primary border border-surface-hover transition-colors"
          >
            取消
          </button>
          <button
            onClick={handleSubmit}
            disabled={busy || !path.trim()}
            className="px-3 py-1.5 rounded-lg text-xs bg-primary/15 text-primary hover:bg-primary/25 transition-colors disabled:opacity-40 disabled:cursor-not-allowed flex items-center gap-1.5"
          >
            {busy && <Loader2 size={12} className="animate-spin" />}
            创建项目
          </button>
        </div>
      </div>
    </div>
  );
}
