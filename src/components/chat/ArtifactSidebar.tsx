import { useCallback, useEffect, useMemo, useState } from 'react';
import {
  ChevronDown,
  ChevronRight,
  Database,
  ExternalLink,
  File,
  FileArchive,
  FileCode2,
  FileImage,
  FileSpreadsheet,
  FileText,
  FolderOpen,
  Loader2,
  Package,
  Presentation,
  RefreshCw,
  Video,
  Save,
  Trash2,
  Upload,
  X,
} from 'lucide-react';
import type { LucideIcon } from 'lucide-react';
import { useDialog } from '@/hooks/useDialog';
import { useSyncTick } from '@/stores/syncTickStore';
import { localArtifactSaveAs, sessionArtifactsSync } from '@/lib/tauri';
import { artifactResolve, artifactSaveAs, openLocalPath, revealInFileManager, sessionArtifactsDelete, sessionArtifactsList, type SessionArtifactItem } from '@/lib/tauri';
import { ArtifactTypeIcon } from './artifactVisual';

type Category = 'document' | 'sheet' | 'slide' | 'image' | 'video' | 'code' | 'data' | 'archive' | 'other';
type TypeFilter = 'all' | 'document' | 'image' | 'video' | 'code' | 'other';

const CATEGORY_ORDER: Category[] = ['document', 'sheet', 'slide', 'image', 'video', 'code', 'data', 'archive', 'other'];
const CATEGORY_META: Record<Category, { label: string; icon: LucideIcon }> = {
  document: { label: '文档', icon: FileText },
  sheet: { label: '表格', icon: FileSpreadsheet },
  slide: { label: '演示', icon: Presentation },
  image: { label: '图片', icon: FileImage },
  video: { label: '视频', icon: Video },
  code: { label: '代码 / 脚本', icon: FileCode2 },
  data: { label: '数据', icon: Database },
  archive: { label: '压缩包', icon: FileArchive },
  other: { label: '其他', icon: File },
};

function categoryOf(ext: string): Category {
  const e = ext.toLowerCase();
  if (['doc', 'docx', 'rtf', 'odt', 'pdf', 'md', 'txt'].includes(e)) return 'document';
  if (['xls', 'xlsx', 'csv', 'ods'].includes(e)) return 'sheet';
  if (['ppt', 'pptx', 'odp'].includes(e)) return 'slide';
  if (['png', 'jpg', 'jpeg', 'gif', 'webp', 'bmp', 'svg', 'avif'].includes(e)) return 'image';
  if (['mp4', 'webm', 'mov', 'm4v', 'mkv', 'avi'].includes(e)) return 'video';
  if (['py', 'ps1', 'sh', 'bash', 'zsh', 'ts', 'tsx', 'js', 'jsx', 'rs', 'go', 'java', 'c', 'cc', 'cpp', 'h', 'hpp', 'html', 'css', 'scss', 'rb', 'php'].includes(e)) return 'code';
  if (['json', 'yaml', 'yml', 'toml', 'xml', 'sql', 'db', 'sqlite'].includes(e)) return 'data';
  if (['zip', 'tar', 'gz', 'tgz', '7z', 'rar', 'bz2', 'xz'].includes(e)) return 'archive';
  return 'other';
}

function formatBytes(bytes: number): string {
  if (!bytes) return '0 B';
  const units = ['B', 'KB', 'MB', 'GB'];
  let value = bytes;
  let index = 0;
  while (value >= 1024 && index < units.length - 1) {
    value /= 1024;
    index += 1;
  }
  return `${index === 0 || value >= 10 ? value.toFixed(0) : value.toFixed(1)} ${units[index]}`;
}

function itemTime(item: SessionArtifactItem): number {
  if (item.modified_at_ms) return item.modified_at_ms;
  const parsed = Date.parse(item.created_at);
  return Number.isNaN(parsed) ? 0 : parsed;
}

function formatTime(item: SessionArtifactItem): string {
  const ms = itemTime(item);
  if (!ms) return '';
  const date = new Date(ms);
  const now = new Date();
  const sameDay = date.toDateString() === now.toDateString();
  return sameDay
    ? date.toLocaleTimeString('zh-CN', { hour: '2-digit', minute: '2-digit' })
    : date.toLocaleDateString('zh-CN', { month: 'numeric', day: 'numeric' });
}

function itemKey(item: SessionArtifactItem): string {
  return `${item.kind}:${item.id}`;
}

interface Props {
  sessionId: string;
  refreshToken: string | number;
  onClose: () => void;
  /** Notify the chat panel that a deliverable footer was removed. */
  onChanged?: () => void;
}

/** Per-session artifact shelf: synced deliverables + local-only process files,
 *  grouped by category, newest first, with multi-select deletion. */
export function ArtifactSidebar({ sessionId, refreshToken, onClose, onChanged }: Props) {
  const { alert, confirm } = useDialog();
  const syncTick = useSyncTick((s) => s.tick);
  const [items, setItems] = useState<SessionArtifactItem[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [typeFilter, setTypeFilter] = useState<TypeFilter>('all');
  const [localOnly, setLocalOnly] = useState(false);
  const [query, setQuery] = useState('');
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [busy, setBusy] = useState(false);
  const [syncing, setSyncing] = useState(false);
  const [collapsed, setCollapsed] = useState<Set<Category>>(new Set());

  const load = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const rows = await sessionArtifactsList(sessionId);
      setItems(rows.sort((a, b) => itemTime(b) - itemTime(a)));
      setSelected(new Set());
    } catch (err) {
      setError(String(err));
    } finally {
      setLoading(false);
    }
  }, [sessionId]);

  useEffect(() => {
    void load();
  }, [load, refreshToken, syncTick]);

  useEffect(() => {
    setSelected(new Set());
  }, [typeFilter, localOnly, query]);

  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    return items.filter((item) => {
      if (localOnly && item.synced) return false;
      if (typeFilter !== 'all') {
        const category = categoryOf(item.ext);
        const typeMatches =
          typeFilter === 'document'
            ? category === 'document' || category === 'sheet' || category === 'slide'
            : typeFilter === 'other'
              ? category === 'data' || category === 'archive' || category === 'other'
              : category === typeFilter;
        if (!typeMatches) return false;
      }
      if (!q) return true;
      return item.name.toLowerCase().includes(q) || item.ext.toLowerCase().includes(q);
    });
  }, [items, typeFilter, localOnly, query]);

  const groups = useMemo(() => {
    const map = new Map<Category, SessionArtifactItem[]>();
    for (const item of filtered) {
      const category = categoryOf(item.ext);
      const list = map.get(category) ?? [];
      list.push(item);
      map.set(category, list);
    }
    return CATEGORY_ORDER.filter((category) => map.has(category)).map((category) => ({
      category,
      items: (map.get(category) ?? []).sort((a, b) => itemTime(b) - itemTime(a)),
    }));
  }, [filtered]);

  const totalBytes = useMemo(() => items.reduce((sum, item) => sum + Math.max(0, item.size), 0), [items]);

  const toggleOne = (item: SessionArtifactItem) => {
    const key = itemKey(item);
    setSelected((current) => {
      const next = new Set(current);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return next;
    });
  };

  const allFilteredSelected = filtered.length > 0 && filtered.every((item) => selected.has(itemKey(item)));
  const localSelectedCount = items.filter((item) => selected.has(itemKey(item)) && item.kind === 'local' && !item.synced).length;
  const toggleAll = () => {
    setSelected((current) => {
      if (allFilteredSelected) {
        const next = new Set(current);
        filtered.forEach((item) => next.delete(itemKey(item)));
        return next;
      }
      const next = new Set(current);
      filtered.forEach((item) => next.add(itemKey(item)));
      return next;
    });
  };

  const openItem = async (item: SessionArtifactItem) => {
    try {
      const path = item.kind === 'synced'
        ? item.uri ? await artifactResolve(item.uri) : null
        : item.path;
      if (!path) throw new Error('产物尚未同步到本机');
      await openLocalPath(path);
    } catch (err) {
      await alert(String(err), { title: '打开产物', kind: 'error' });
    }
  };

  const revealItem = async (item: SessionArtifactItem) => {
    try {
      const path = item.kind === 'synced'
        ? item.uri ? await artifactResolve(item.uri) : null
        : item.path;
      if (!path) throw new Error('产物尚未同步到本机');
      await revealInFileManager(path);
    } catch (err) {
      await alert(String(err), { title: '在文件夹中显示', kind: 'error' });
    }
  };

  const saveItem = async (item: SessionArtifactItem) => {
    if (item.kind === 'synced' && !item.uri) return;
    try {
      const { save } = await import('@tauri-apps/plugin-dialog');
      const target = await save({
        defaultPath: item.name,
        filters: item.ext ? [{ name: item.ext.toUpperCase(), extensions: [item.ext] }] : undefined,
      });
      if (!target) return;
      if (item.kind === 'local') {
        await localArtifactSaveAs(sessionId, item.id, target);
      } else if (item.uri) {
        await artifactSaveAs(item.uri, target);
      }
      await alert(`已保存到：${target}`, { title: '另存为', kind: 'success' });
    } catch (err) {
      await alert(String(err), { title: '另存为', kind: 'error' });
    }
  };

  const syncSelected = async () => {
    const targets = items.filter(
      (item) => selected.has(itemKey(item)) && item.kind === 'local' && !item.synced,
    );
    if (targets.length === 0 || syncing || busy) return;
    const total = targets.reduce((sum, item) => sum + Math.max(0, item.size), 0);
    const ok = await confirm(
      `将选中的 ${targets.length} 个本地文件（${formatBytes(total)}）同步到其他设备？\n\n` +
        '同步会复制一份到 blob，并创建跨设备产物索引；本地原文件保留。',
      { title: '同步产物' },
    );
    if (!ok) return;
    setSyncing(true);
    setError(null);
    try {
      const result = await sessionArtifactsSync(sessionId, targets.map((item) => item.id));
      await load();
      useSyncTick.getState().bump();
      onChanged?.();
      if (result.errors.length > 0) {
        await alert(`已同步 ${result.synced} 个；失败 ${result.errors.length} 个：\n${result.errors[0]}`, {
          title: '同步产物',
          kind: 'warning',
        });
      }
    } catch (err) {
      setError(String(err));
    } finally {
      setSyncing(false);
    }
  };

  const deleteSelected = async () => {
    const targets = items.filter((item) => selected.has(itemKey(item)));
    if (targets.length === 0 || busy) return;
    const hasSynced = targets.some((item) => item.synced);
    const ok = await confirm(
      `确定删除选中的 ${targets.length} 个产物？\n\n` +
        '仅本地文件会移到应用回收站；' +
        (hasSynced ? '已同步文件会从会话索引移除，blob 由存储清理后续回收。' : ''),
      { title: '删除产物', danger: true },
    );
    if (!ok) return;
    setBusy(true);
    setError(null);
    try {
      const result = await sessionArtifactsDelete(
        sessionId,
        targets.map((item) => ({ id: item.id, kind: item.kind })),
      );
      await load();
      useSyncTick.getState().bump();
      onChanged?.();
      if (result.errors.length > 0) {
        await alert(`已删除 ${result.deleted} 个；失败 ${result.errors.length} 个：\n${result.errors[0]}`, {
          title: '删除产物',
          kind: 'warning',
        });
      }
    } catch (err) {
      setError(String(err));
    } finally {
      setBusy(false);
    }
  };

  const toggleCategory = (category: Category) => {
    setCollapsed((current) => {
      const next = new Set(current);
      if (next.has(category)) next.delete(category);
      else next.add(category);
      return next;
    });
  };

  return (
    <aside className="flex h-full w-[340px] max-w-[82vw] shrink-0 flex-col border-l border-codex-border bg-codex-surface">
      <div className="flex shrink-0 items-center gap-2 border-b border-codex-border px-3 py-2.5">
        <Package size={15} className="text-codex-muted" />
        <div className="min-w-0 flex-1">
          <div className="text-[12px] font-medium text-codex-primary">会话产物</div>
          <div className="text-[10px] text-codex-muted">
            {items.length} 个 · {formatBytes(totalBytes)}
            {items.some((item) => !item.synced) ? ' · 仅本地文件不同步' : ''}
          </div>
        </div>
        <button
          type="button"
          onClick={() => void load()}
          disabled={loading}
          className="rounded p-1 text-codex-muted hover:bg-codex-hover hover:text-codex-primary disabled:opacity-50"
          title="刷新产物"
        >
          <RefreshCw size={13} className={loading ? 'animate-spin' : ''} />
        </button>
        <button
          type="button"
          onClick={onClose}
          className="rounded p-1 text-codex-muted hover:bg-codex-hover hover:text-codex-primary"
          title="关闭产物侧栏"
        >
          <X size={14} />
        </button>
      </div>

      <div className="shrink-0 space-y-2 border-b border-codex-border px-3 py-2">
        <div className="flex flex-wrap gap-1">
          {([
            ['all', '全部'],
            ['document', '文档'],
            ['image', '图片'],
            ['video', '视频'],
            ['code', '代码/脚本'],
            ['other', '其它'],
          ] as const).map(([key, label]) => (
            <button
              key={key}
              type="button"
              onClick={() => setTypeFilter(key)}
              className={`rounded px-2 py-1 text-[10px] ${
                typeFilter === key
                  ? 'bg-codex-hover text-codex-primary'
                  : 'text-codex-muted hover:text-codex-primary'
              }`}
            >
              {label}
            </button>
          ))}
        </div>
        <label className="flex items-center gap-1.5 text-[10px] text-codex-muted">
          <input
            type="checkbox"
            checked={localOnly}
            onChange={(e) => setLocalOnly(e.target.checked)}
            className="h-3 w-3 accent-codex-accent"
          />
          仅本地
          <span className="text-codex-muted/70">
            {items.filter((item) => !item.synced).length}
          </span>
        </label>
        <input
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          placeholder="搜索文件名…"
          className="w-full rounded-md border border-codex-border bg-codex-bg/40 px-2 py-1 text-[11px] text-codex-primary outline-none placeholder:text-codex-muted focus:border-codex-accent/60"
        />
      </div>

      <div className="min-h-0 flex-1 overflow-y-auto px-2 py-2">
        {error && <div className="m-2 rounded border border-red-500/30 bg-red-500/10 p-2 text-[10px] text-red-300">{error}</div>}
        {loading && items.length === 0 && (
          <div className="flex items-center justify-center py-8 text-codex-muted">
            <Loader2 size={15} className="animate-spin" />
          </div>
        )}
        {!loading && filtered.length === 0 && (
          <div className="py-8 text-center text-[11px] text-codex-muted">没有可显示的产物</div>
        )}
        {groups.map(({ category, items: groupItems }) => {
          const meta = CATEGORY_META[category];
          const Icon = meta.icon;
          const isCollapsed = collapsed.has(category);
          return (
            <section key={category} className="mb-2">
              <button
                type="button"
                onClick={() => toggleCategory(category)}
                className="flex w-full items-center gap-1 rounded px-1 py-1 text-[10px] font-medium text-codex-muted hover:bg-codex-hover/60"
              >
                {isCollapsed ? <ChevronRight size={11} /> : <ChevronDown size={11} />}
                <Icon size={11} />
                <span>{meta.label}</span>
                <span className="text-codex-muted/70">{groupItems.length}</span>
              </button>
              {!isCollapsed && (
                <div className="space-y-0.5">
                  {groupItems.map((item) => {
                    const key = itemKey(item);
                    const checked = selected.has(key);
                    return (
                      <div
                        key={key}
                        className={`group flex items-center gap-1.5 rounded-md px-1.5 py-1 hover:bg-codex-hover/70 ${
                          checked ? 'bg-codex-hover/50' : ''
                        }`}
                      >
                        <input
                          type="checkbox"
                          checked={checked}
                          onChange={() => toggleOne(item)}
                          className="h-3 w-3 shrink-0 accent-codex-accent"
                          aria-label={`选择 ${item.name}`}
                        />
                        <ArtifactTypeIcon ext={item.ext} size={12} />
                        <div className="min-w-0 flex-1">
                          <div className="truncate text-[11px] text-codex-primary" title={item.name}>
                            {item.name}
                          </div>
                          <div className="flex items-center gap-1 text-[9px] text-codex-muted">
                            <span>{formatBytes(item.size)}</span>
                            {formatTime(item) && <span>· {formatTime(item)}</span>}
                            <span className={item.synced ? 'text-emerald-400/80' : 'text-amber-400/80'}>
                              {item.synced ? '已同步' : '仅本地'}
                            </span>
                          </div>
                        </div>
                        <div className="flex shrink-0 items-center gap-0.5 opacity-100 sm:opacity-0 sm:group-hover:opacity-100">
                          <button
                            type="button"
                            onClick={() => void openItem(item)}
                            className="rounded p-0.5 text-codex-muted hover:text-codex-primary"
                            title="打开"
                          >
                            <ExternalLink size={11} />
                          </button>
                          {(
                            <button
                              type="button"
                              onClick={() => void saveItem(item)}
                              className="rounded p-0.5 text-codex-muted hover:text-codex-primary"
                              title="另存为"
                            >
                              <Save size={11} />
                            </button>
                          )}
                          <button
                            type="button"
                            onClick={() => void revealItem(item)}
                            className="rounded p-0.5 text-codex-muted hover:text-codex-primary"
                            title="在文件夹中显示"
                          >
                            <FolderOpen size={11} />
                          </button>
                        </div>
                      </div>
                    );
                  })}
                </div>
              )}
            </section>
          );
        })}
      </div>

      <div className="flex shrink-0 items-center gap-2 border-t border-codex-border px-3 py-2">
        <label className="flex items-center gap-1 text-[10px] text-codex-muted">
          <input
            type="checkbox"
            checked={allFilteredSelected}
            onChange={toggleAll}
            disabled={filtered.length === 0}
            className="h-3 w-3 accent-codex-accent"
          />
          全选
        </label>
        <span className="text-[10px] text-codex-muted">已选 {selected.size}</span>
        <button
          type="button"
          onClick={() => void syncSelected()}
          disabled={localSelectedCount === 0 || syncing || busy}
          className="ml-auto inline-flex items-center gap-1 rounded-md border border-sky-500/30 bg-sky-500/10 px-2 py-1 text-[10px] text-sky-300 hover:bg-sky-500/20 disabled:opacity-40"
        >
          {syncing ? <Loader2 size={11} className="animate-spin" /> : <Upload size={11} />}
          同步
        </button>
        <button
          type="button"
          onClick={() => void deleteSelected()}
          disabled={selected.size === 0 || syncing || busy}
          className="inline-flex items-center gap-1 rounded-md border border-red-500/30 bg-red-500/10 px-2 py-1 text-[10px] text-red-300 hover:bg-red-500/20 disabled:opacity-40"
        >
          {busy ? <Loader2 size={11} className="animate-spin" /> : <Trash2 size={11} />}
          删除
        </button>
      </div>
    </aside>
  );
}
