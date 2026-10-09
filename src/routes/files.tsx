import { useState, useEffect, useCallback } from 'react';
import { createRoute } from '@tanstack/react-router';
import { Route as RootRoute } from './__root';
import { Folder, File, ChevronRight, Home, FileText, Image, Loader2, Database } from 'lucide-react';
import { homeDir, desktopDir, documentDir, downloadDir, appDataDir } from '@tauri-apps/api/path';
import { fileBrowserListDir, fileBrowserOpenInSystem } from '@/lib/tauri';
import type { FileEntry } from '@/lib/types';

/** Last browsed directory is restored on the next visit (device-local). */
const CWD_KEY = 'siku.files.cwd';

/** Strip trailing path separators so breadcrumbs stay clean ("C:\" keeps its sep). */
function trimSep(p: string): string {
  const t = p.replace(/[\\/]+$/, '');
  return t || p;
}

interface QuickPath {
  label: string;
  path: string;
  muted?: boolean;
}

function FilesPage() {
  const [cwd, setCwd] = useState('');
  const [home, setHome] = useState('');
  const [quickPaths, setQuickPaths] = useState<QuickPath[]>([]);
  const [entries, setEntries] = useState<FileEntry[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Resolve well-known directories once, then pick the initial directory:
  // last browsed (if any) else the user home — never the drive root.
  useEffect(() => {
    (async () => {
      const [h, desk, docs, dl, appData] = await Promise.all([
        homeDir(), desktopDir(), documentDir(), downloadDir(), appDataDir(),
      ]);
      const home = trimSep(h);
      setHome(home);
      setQuickPaths([
        { label: '主目录', path: home },
        { label: '桌面', path: trimSep(desk) },
        { label: '文档', path: trimSep(docs) },
        { label: '下载', path: trimSep(dl) },
        { label: '应用数据', path: trimSep(appData), muted: true },
      ]);
      setCwd(localStorage.getItem(CWD_KEY) || home);
    })().catch(() => setError('无法获取系统目录'));
  }, []);

  const loadDir = useCallback(async (path: string) => {
    setLoading(true); setError(null);
    try {
      setEntries(await fileBrowserListDir(path, false));
      localStorage.setItem(CWD_KEY, path);
    } catch (err) {
      setError(`${err}`);
      setEntries([]);
      return false;
    } finally {
      setLoading(false);
    }
    return true;
  }, []);

  useEffect(() => {
    if (!cwd) return;
    // A stale saved path (renamed/removed dir, or a drive that no longer
    // exists) falls back to the home directory instead of showing an error.
    loadDir(cwd).then((ok) => {
      if (!ok && home && cwd !== home) setCwd(home);
    });
  }, [cwd, home, loadDir]);

  const handleClick = (entry: FileEntry) => {
    if (entry.is_dir) { setCwd(entry.path); }
  };

  const handleOpen = async (entry: FileEntry) => {
    if (!entry.is_dir) {
      try { await fileBrowserOpenInSystem(entry.path); }
      catch (err) { setError(`${err}`); }
    }
  };

  const getIcon = (entry: FileEntry) => {
    if (entry.is_dir) return <Folder size={16} className="text-primary" />;
    if (entry.mime_type?.startsWith('image/')) return <Image size={16} className="text-accent" />;
    if (entry.mime_type?.startsWith('text/')) return <FileText size={16} className="text-text-secondary" />;
    return <File size={16} className="text-text-secondary" />;
  };

  const sep = cwd.includes('\\') ? '\\' : '/';
  const pathParts = cwd.split(sep).filter(Boolean);
  const isWindows = sep === '\\';

  const buildPath = (idx: number) => {
    if (isWindows) {
      return pathParts.slice(0, idx + 1).join('\\') + (idx === 0 ? '\\' : '');
    }
    return '/' + pathParts.slice(0, idx + 1).join('/');
  };

  return (
    <div className="flex h-full flex-col">
      <div className="flex items-center gap-1 px-4 py-2 border-b border-surface-hover text-xs">
        <button
          onClick={() => home && setCwd(home)}
          className="p-1 rounded hover:bg-surface-hover"
          title="主目录"
        >
          <Home size={14} className="text-text-secondary" />
        </button>
        {quickPaths.map((q) => (
          <button
            key={q.label}
            onClick={() => setCwd(q.path)}
            className={`flex items-center gap-1 px-1.5 py-0.5 rounded transition-colors ${
              cwd === q.path
                ? 'text-primary bg-primary/10'
                : q.muted
                  ? 'text-text-secondary/60 hover:text-text-secondary hover:bg-surface-hover'
                  : 'text-text-secondary hover:text-text-primary hover:bg-surface-hover'
            }`}
          >
            {q.muted && <Database size={11} />}
            {q.label}
          </button>
        ))}
        <span className="mx-1 h-3.5 w-px bg-surface-hover" />
        {pathParts.map((part, i) => (
          <span key={i} className="flex items-center gap-1">
            {i > 0 && <ChevronRight size={12} className="text-text-secondary/50" />}
            <button
              onClick={() => setCwd(buildPath(i))}
              className="hover:text-primary"
            >
              {part}
            </button>
          </span>
        ))}
      </div>

      {error && <div className="px-4 py-2 text-xs text-red-400 bg-red-500/5">{error}</div>}

      <div className="flex-1 overflow-y-auto p-2">
        {loading ? (
          <div className="flex justify-center py-8"><Loader2 size={20} className="animate-spin text-text-secondary" /></div>
        ) : (
          <div className="space-y-0.5">
            {entries.map((entry) => (
              <div
                key={entry.path}
                onClick={() => handleClick(entry)}
                onDoubleClick={() => handleOpen(entry)}
                className={`flex items-center gap-3 px-3 py-2 rounded-lg cursor-pointer text-sm transition-colors ${
                  entry.is_dir ? 'hover:bg-primary/5' : 'hover:bg-surface-hover'
                }`}
              >
                {getIcon(entry)}
                <span className="flex-1 truncate text-text-primary">{entry.name}</span>
                {!entry.is_dir && (
                  <span className="text-xs text-text-secondary/60">{formatSize(entry.size)}</span>
                )}
              </div>
            ))}
          </div>
        )}
      </div>
    </div>
  );
}

function formatSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  if (bytes < 1024 * 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  return `${(bytes / (1024 * 1024 * 1024)).toFixed(1)} GB`;
}

export const Route = createRoute({
  getParentRoute: () => RootRoute,
  path: '/files',
  component: FilesPage,
});
