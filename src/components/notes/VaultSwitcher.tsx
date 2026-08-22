import { useEffect, useRef, useState } from 'react';
import { Check, Plus, Pencil, Trash2, X, Database, Download, Upload } from 'lucide-react';
import { useDialog } from '@/hooks/useDialog';
import type { Vault } from '@/lib/types';

interface Props {
  vaults: Vault[];
  currentVaultId: string | null;
  onSwitch: (id: string) => void;
  onCreate: (name: string) => void;
  onRename: (id: string, name: string) => void;
  onDelete: (id: string) => void;
  /** Export the current vault to Markdown files (folder picked by caller). */
  onExport?: () => void;
  /** Import Markdown notes from a folder into the current vault. */
  onImport?: () => void;
  onClose: () => void;
}

/** Obsidian-style vault switcher dialog. */
export function VaultSwitcher({ vaults, currentVaultId, onSwitch, onCreate, onRename, onDelete, onExport, onImport, onClose }: Props) {
  const { confirm } = useDialog();
  const [creating, setCreating] = useState(false);
  const [newName, setNewName] = useState('');
  const [renamingId, setRenamingId] = useState<string | null>(null);
  const [renameValue, setRenameValue] = useState('');
  const inputRef = useRef<HTMLInputElement>(null);
  const panelRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const onDown = (e: MouseEvent) => {
      if (panelRef.current && !panelRef.current.contains(e.target as Node)) onClose();
    };
    window.addEventListener('mousedown', onDown);
    return () => window.removeEventListener('mousedown', onDown);
  }, [onClose]);

  useEffect(() => {
    if (creating || renamingId !== null) setTimeout(() => inputRef.current?.focus(), 30);
  }, [creating, renamingId]);

  const commitCreate = () => {
    const name = newName.trim();
    if (name) onCreate(name);
    setCreating(false);
    setNewName('');
  };

  const commitRename = () => {
    if (renamingId !== null) {
      const name = renameValue.trim();
      if (name) onRename(renamingId, name);
    }
    setRenamingId(null);
    setRenameValue('');
  };

  const handleDelete = async (v: Vault) => {
    const ok = await confirm(`确定删除库「${v.name}」吗？库中的所有笔记将一并删除，此操作不可撤销。`);
    if (ok) onDelete(v.id);
  };

  return (
    <div className="fixed inset-0 z-[200] flex items-center justify-center">
      <div className="absolute inset-0 bg-black/50 backdrop-blur-sm" onClick={onClose} />
      <div
        ref={panelRef}
        className="relative w-[340px] max-h-[70vh] flex flex-col bg-surface border border-surface-hover rounded-xl shadow-2xl overflow-hidden"
      >
        <div className="flex items-center justify-between px-4 py-3 border-b border-surface-hover">
          <span className="text-sm font-medium text-text-primary">库切换器</span>
          <button
            onClick={onClose}
            className="p-1 rounded text-text-secondary/60 hover:text-text-primary hover:bg-surface-hover transition-colors"
            aria-label="关闭"
          >
            <X size={14} />
          </button>
        </div>

        <div className="flex-1 overflow-y-auto py-1">
          {vaults.length === 0 && (
            <p className="text-xs text-text-secondary/60 text-center py-6">暂无库</p>
          )}
          {vaults.map((v) => {
            const isCurrent = v.id === currentVaultId;
            const isRenaming = renamingId === v.id;
            return (
              <div
                key={v.id}
                onClick={() => !isRenaming && onSwitch(v.id)}
                className={`group flex items-center gap-2 px-3 py-2 text-[13px] cursor-pointer transition-colors ${
                  isCurrent ? 'bg-primary/10 text-text-primary' : 'text-text-secondary hover:bg-surface-hover'
                }`}
              >
                <Database size={14} className="shrink-0 text-text-secondary/60" />
                {isRenaming ? (
                  <input
                    ref={inputRef}
                    autoFocus
                    value={renameValue}
                    onChange={(e) => setRenameValue(e.target.value)}
                    onBlur={commitRename}
                    onKeyDown={(e) => {
                      if (e.key === 'Enter') commitRename();
                      if (e.key === 'Escape') {
                        setRenamingId(null);
                        setRenameValue('');
                      }
                      e.stopPropagation();
                    }}
                    onClick={(e) => e.stopPropagation()}
                    className="flex-1 min-w-0 bg-background text-text-primary text-[13px] px-1 py-0.5 rounded border border-primary/30 focus:outline-none"
                  />
                ) : (
                  <span className="flex-1 truncate">{v.name}</span>
                )}
                {isCurrent && <Check size={14} className="shrink-0 text-primary" />}
                {!isRenaming && (
                  <span className="flex items-center gap-0.5 opacity-0 group-hover:opacity-100">
                    <button
                      onClick={(e) => {
                        e.stopPropagation();
                        setRenamingId(v.id);
                        setRenameValue(v.name);
                      }}
                      className="p-1 rounded text-text-secondary/60 hover:text-text-primary hover:bg-surface-hover transition-colors"
                      title="重命名库"
                    >
                      <Pencil size={12} />
                    </button>
                    <button
                      onClick={(e) => {
                        e.stopPropagation();
                        handleDelete(v);
                      }}
                      className="p-1 rounded text-text-secondary/60 hover:text-red-400 hover:bg-red-500/10 transition-colors"
                      title="删除库"
                    >
                      <Trash2 size={12} />
                    </button>
                  </span>
                )}
              </div>
            );
          })}
        </div>

        <div className="px-3 py-2 border-t border-surface-hover flex flex-col gap-1">
          {creating ? (
            <div className="flex items-center gap-2">
              <input
                ref={inputRef}
                autoFocus
                value={newName}
                onChange={(e) => setNewName(e.target.value)}
                onBlur={commitCreate}
                onKeyDown={(e) => {
                  if (e.key === 'Enter') commitCreate();
                  if (e.key === 'Escape') {
                    setCreating(false);
                    setNewName('');
                  }
                }}
                placeholder="输入库名称"
                className="flex-1 min-w-0 h-8 bg-background text-text-primary text-[13px] px-2 rounded border border-surface-hover focus:border-primary/50 focus:outline-none placeholder:text-text-secondary/40"
              />
              <button
                onClick={commitCreate}
                className="px-2.5 h-8 rounded bg-primary/10 text-primary text-xs font-medium hover:bg-primary/20 transition-colors"
              >
                创建
              </button>
            </div>
          ) : (
            <button
              onClick={() => setCreating(true)}
              className="w-full flex items-center justify-center gap-1.5 h-8 rounded text-xs text-text-secondary hover:text-text-primary hover:bg-surface-hover transition-colors"
            >
              <Plus size={13} /> 新建库
            </button>
          )}
          {(onExport || onImport) && (
            <div className="flex items-center gap-1.5 pt-1.5 mt-0.5 border-t border-surface-hover">
              {onExport && (
                <button
                  onClick={onExport}
                  className="flex-1 flex items-center justify-center gap-1.5 h-7 rounded text-[11px] text-text-secondary hover:text-text-primary hover:bg-surface-hover transition-colors"
                  title="将当前库导出为 Markdown 文件"
                >
                  <Download size={12} /> 导出库
                </button>
              )}
              {onImport && (
                <button
                  onClick={onImport}
                  className="flex-1 flex items-center justify-center gap-1.5 h-7 rounded text-[11px] text-text-secondary hover:text-text-primary hover:bg-surface-hover transition-colors"
                  title="从 Markdown 文件夹导入笔记到当前库"
                >
                  <Upload size={12} /> 导入库
                </button>
              )}
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
