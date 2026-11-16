import { useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react';
import {
  ChevronDown,
  ChevronUp,
  Database,
  ExternalLink,
  File,
  FileArchive,
  FileCode2,
  FileSpreadsheet,
  FileImage,
  FileText,
  FolderOpen,
  Loader2,
  Presentation,
  Save,
  Video,
} from 'lucide-react';
import type { LucideIcon } from 'lucide-react';
import { useDialog } from '@/hooks/useDialog';
import {
  artifactResolve,
  artifactSaveAs,
  localArtifactSaveAs,
  openLocalPath,
  revealInFileManager,
  sessionTurnFiles,
  type SessionArtifactItem,
} from '@/lib/tauri';
import { useSyncTick } from '@/stores/syncTickStore';
import type { ArtifactEntry } from '@/lib/artifactSection';
import { ArtifactTypeIcon } from './artifactVisual';

type Category = 'document' | 'sheet' | 'slide' | 'image' | 'video' | 'code' | 'data' | 'archive' | 'other';
type DisplayArtifact = SessionArtifactItem & { legacy?: boolean };

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

function useGridColumns(ref: React.RefObject<HTMLDivElement | null>, dependency: unknown): number {
  const [columns, setColumns] = useState(2);
  useLayoutEffect(() => {
    const grid = ref.current;
    if (!grid) return;
    const measure = () => {
      const count = window
        .getComputedStyle(grid)
        .gridTemplateColumns.split(/\s+/)
        .filter(Boolean).length;
      if (count > 0) setColumns(count);
    };
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(grid);
    return () => observer.disconnect();
  }, [ref, dependency]);
  return columns;
}

function normalizeLegacy(artifacts: ArtifactEntry[]): DisplayArtifact[] {
  return artifacts.map((artifact) => ({
    id: `legacy:${artifact.uri}`,
    kind: 'synced',
    synced: true,
    message_id: '',
    name: artifact.label,
    ext: artifact.ext,
    size: 0,
    path: null,
    uri: artifact.uri,
    modified_at_ms: null,
    created_at: '',
    legacy: true,
  }));
}

function ArtifactCard({ item, sessionId }: { item: DisplayArtifact; sessionId?: string }) {
  const { alert } = useDialog();
  const [saving, setSaving] = useState(false);
  const [path, setPath] = useState<string | null>(item.path);
  const [failed, setFailed] = useState(false);

  useEffect(() => {
    let live = true;
    setPath(item.path);
    setFailed(false);
    if (item.path || !item.uri) return;
    artifactResolve(item.uri)
      .then((resolved) => {
        if (live) setPath(resolved);
      })
      .catch(() => {
        if (live) setFailed(true);
      });
    return () => {
      live = false;
    };
  }, [item.path, item.uri]);

  const open = () => {
    if (!path) {
      void alert(failed ? '产物尚未同步到本机，请稍后重试' : '产物正在准备，请稍后重试', {
        title: '打开产物',
        kind: failed ? 'warning' : 'info',
      });
      return;
    }
    void openLocalPath(path).catch((err) => alert(String(err), { title: '打开产物', kind: 'error' }));
  };

  const reveal = () => {
    if (path) void revealInFileManager(path).catch((err) => alert(String(err), { title: '在文件夹中显示', kind: 'error' }));
  };

  const saveAs = async () => {
    if (saving) return;
    setSaving(true);
    try {
      const { save } = await import('@tauri-apps/plugin-dialog');
      const target = await save({
        defaultPath: item.name,
        filters: item.ext ? [{ name: item.ext.toUpperCase(), extensions: [item.ext] }] : undefined,
      });
      if (!target) return;
      if (item.kind === 'synced' && item.uri) {
        await artifactSaveAs(item.uri, target);
      } else if (sessionId) {
        await localArtifactSaveAs(sessionId, item.id, target);
      } else {
        throw new Error('本地产物缺少会话 ID');
      }
      await alert(`已保存到：${target}`, { title: '另存为', kind: 'success' });
    } catch (err) {
      await alert(String(err), { title: '另存为', kind: 'error' });
    } finally {
      setSaving(false);
    }
  };

  return (
    <div className="group flex min-w-0 items-center gap-2 rounded-lg border border-codex-border/60 bg-codex-bg/30 px-2 py-1.5">
      <ArtifactTypeIcon ext={item.ext} size={17} />
      <button type="button" onClick={open} className="min-w-0 flex-1 text-left" title={item.name}>
        <div className="truncate text-[11px] leading-snug text-codex-primary">{item.name}</div>
        <div className="mt-0.5 flex flex-wrap items-center gap-x-1.5 gap-y-0.5 text-[9px] text-codex-muted">
          <span>{formatBytes(item.size)}</span>
          <span className={item.synced ? 'text-emerald-400/80' : 'text-amber-400/80'}>
            {item.synced ? '已同步' : '仅本地'}
          </span>
        </div>
      </button>
      <div className="flex shrink-0 items-center gap-0.5 opacity-100 sm:opacity-0 sm:group-hover:opacity-100">
        <button
          type="button"
          onClick={() => void saveAs()}
          disabled={saving}
          className="rounded p-0.5 text-codex-muted hover:text-codex-primary disabled:opacity-50"
          title="另存为"
        >
          {saving ? <Loader2 size={11} className="animate-spin" /> : <Save size={11} />}
        </button>
        {path && (
          <button
            type="button"
            onClick={reveal}
            className="rounded p-0.5 text-codex-muted hover:text-codex-primary"
            title="在文件夹中显示"
          >
            <FolderOpen size={11} />
          </button>
        )}
        <button
          type="button"
          onClick={open}
          className="rounded p-0.5 text-codex-muted hover:text-codex-primary"
          title="打开"
        >
          <ExternalLink size={11} />
        </button>
      </div>
    </div>
  );
}

function CategoryGroup({ category, items, sessionId }: { category: Category; items: DisplayArtifact[]; sessionId?: string }) {
  const gridRef = useRef<HTMLDivElement>(null);
  const columns = useGridColumns(gridRef, items.length);
  const [expanded, setExpanded] = useState(false);
  const limit = columns * 2;
  const visible = expanded ? items : items.slice(0, limit);
  const hidden = items.length - visible.length;
  const meta = CATEGORY_META[category];
  const Icon = meta.icon;

  return (
    <div className="mt-2 first:mt-0">
      <div className="mb-1 flex items-center gap-1.5 text-[10px] font-medium text-codex-muted">
        <Icon size={11} />
        <span>{meta.label}</span>
        <span className="text-codex-muted/70">{items.length}</span>
      </div>
      <div ref={gridRef} className="grid gap-1.5 [grid-template-columns:repeat(auto-fill,minmax(190px,1fr))]">
        {visible.map((item) => (
          <ArtifactCard key={`${item.kind}:${item.id}`} item={item} sessionId={sessionId} />
        ))}
      </div>
      {(items.length > limit || expanded) && (
        <button
          type="button"
          onClick={() => setExpanded((v) => !v)}
          className="mt-1 inline-flex items-center gap-0.5 text-[10px] text-codex-muted hover:text-codex-primary"
        >
          {expanded ? <ChevronUp size={11} /> : <ChevronDown size={11} />}
          {expanded ? '收起' : `展开余下 ${hidden} 个`}
        </button>
      )}
    </div>
  );
}

interface Props {
  artifacts?: ArtifactEntry[];
  sessionId?: string;
  messageId?: string;
}

/** In-message artifact shelf. It always shows the turn's local files; synced
 *  artifacts from a peer replace/augment them when no local copy exists. */
export function ArtifactSection({ artifacts = [], sessionId, messageId }: Props) {
  const syncTick = useSyncTick((s) => s.tick);
  const [items, setItems] = useState<DisplayArtifact[]>(() => normalizeLegacy(artifacts));
  const [loading, setLoading] = useState(false);

  useEffect(() => {
    if (!sessionId || !messageId) {
      setItems(normalizeLegacy(artifacts));
      return;
    }
    let live = true;
    setLoading(true);
    sessionTurnFiles(sessionId, messageId)
      .then((rows) => {
        if (!live) return;
        setItems(rows.length > 0 ? rows : normalizeLegacy(artifacts));
      })
      .catch(() => {
        if (live) setItems(normalizeLegacy(artifacts));
      })
      .finally(() => {
        if (live) setLoading(false);
      });
    return () => {
      live = false;
    };
    // artifacts is a parser fallback for legacy messages only.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [sessionId, messageId, syncTick, artifacts.length]);

  const groups = useMemo(() => {
    const map = new Map<Category, DisplayArtifact[]>();
    for (const item of items) {
      const category = categoryOf(item.ext);
      const list = map.get(category) ?? [];
      list.push(item);
      map.set(category, list);
    }
    return CATEGORY_ORDER.filter((category) => map.has(category)).map((category) => ({
      category,
      items: map.get(category) ?? [],
    }));
  }, [items]);

  if (items.length === 0) return loading ? null : null;

  return (
    <section className="not-prose mt-3 rounded-lg border border-codex-border/60 bg-codex-hover/15 p-2.5">
      <div className="mb-2 flex items-center gap-1.5 text-[11px] font-medium text-codex-muted">
        <FileText size={13} className="text-codex-muted" />
        <span>本轮产物</span>
        <span className="rounded-full bg-codex-hover px-1.5 py-0.5 text-[10px] leading-none text-codex-muted">
          {items.length}
        </span>
      </div>
      {groups.map(({ category, items: groupItems }) => (
        <CategoryGroup key={category} category={category} items={groupItems} sessionId={sessionId} />
      ))}
    </section>
  );
}
