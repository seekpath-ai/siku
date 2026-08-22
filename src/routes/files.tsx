import { useState, useEffect } from 'react';
import { createRoute } from '@tanstack/react-router';
import { Route as RootRoute } from './__root';
import { Folder, File, ChevronRight, Home, FileText, Image, Loader2 } from 'lucide-react';
import { fileBrowserListDir, fileBrowserOpenInSystem } from '@/lib/tauri';
import type { FileEntry } from '@/lib/types';

function getDefaultPath(): string {
  // Windows: use C:\ or USERPROFILE; Linux/macOS: use /home or $HOME
  if (typeof navigator !== 'undefined' && navigator.userAgent.includes('Windows')) {
    return 'C:\\';
  }
  return '/home';
}

function FilesPage() {
  const [cwd, setCwd] = useState(getDefaultPath());
  const [entries, setEntries] = useState<FileEntry[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => { loadDir(cwd); }, [cwd]);

  const loadDir = async (path: string) => {
    setLoading(true); setError(null);
    try { setEntries(await fileBrowserListDir(path, false)); }
    catch (err) { setError(`${err}`); }
    finally { setLoading(false); }
  };

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
        <button onClick={() => setCwd(isWindows ? 'C:\\' : '/')} className="p-1 rounded hover:bg-surface-hover">
          <Home size={14} className="text-text-secondary" />
        </button>
        {pathParts.map((part, i) => (
          <span key={i} className="flex items-center gap-1">
            <ChevronRight size={12} className="text-text-secondary/50" />
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
