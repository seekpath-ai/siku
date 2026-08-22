import { Download, FileCheck, Eye, ExternalLink, Loader2, CheckCheck, Clock } from 'lucide-react';
import type { ResearchSource } from '@/lib/types';
import { isoToDisplay } from '@/lib/time';

interface Props {
  sources: ResearchSource[];
  /** Source id currently being imported (shows a spinner). */
  importingId?: string | null;
  onImport?: (source: ResearchSource) => void;
  onMarkRead?: (source: ResearchSource) => void;
}

const statusIcons: Record<string, React.ReactNode> = {
  discovered: <Eye size={12} className="text-yellow-400" />,
  downloaded: <Download size={12} className="text-blue-400" />,
  imported: <FileCheck size={12} className="text-accent" />,
  read: <CheckCheck size={12} className="text-text-secondary" />,
};

const statusLabel: Record<string, string> = {
  discovered: '已发现',
  downloaded: '已下载',
  imported: '已导入',
  read: '已读',
};

export function SourceTable({ sources, importingId, onImport, onMarkRead }: Props) {
  if (sources.length === 0) {
    return <p className="text-sm text-text-secondary py-4">暂无发现的论文。点击"开始发现"搜索前沿研究。</p>;
  }

  return (
    <div className="space-y-1">
      {sources.map((src) => {
        const importing = importingId === src.id;
        const canImport = (src.status === 'discovered' || src.status === 'downloaded') && !!onImport;
        const canMarkRead = src.status === 'imported' && !!onMarkRead;
        return (
          <div key={src.id} className="flex items-center gap-3 p-3 bg-surface border border-surface-hover rounded-lg text-sm">
            <div className="flex-1 min-w-0">
              <div className="flex items-center gap-2">
                <span className="text-xs px-1.5 py-0.5 rounded bg-surface-hover text-text-secondary">{src.source_type}</span>
                <span className="flex items-center gap-1 text-xs text-text-secondary">
                  {statusIcons[src.status]}{statusLabel[src.status]}
                </span>
                {src.discovered_at && (
                  <span className="flex items-center gap-1 text-[11px] text-text-secondary/50">
                    <Clock size={10} />
                    {isoToDisplay(src.discovered_at)}
                  </span>
                )}
              </div>
              <p className="font-medium text-text-primary mt-1 truncate">{src.title || 'Untitled'}</p>
              {src.authors && <p className="text-xs text-text-secondary mt-0.5 truncate">{src.authors}</p>}
            </div>
            <div className="flex items-center gap-1 shrink-0">
              {canImport && (
                <button
                  onClick={() => onImport?.(src)}
                  disabled={importing}
                  className="flex items-center gap-1 px-2 py-1 rounded bg-primary/10 text-primary text-xs hover:bg-primary/20 disabled:opacity-50 transition-colors"
                  title="导入到图书馆"
                >
                  {importing ? <Loader2 size={12} className="animate-spin" /> : <Download size={12} />}
                  {importing ? '导入中…' : '导入'}
                </button>
              )}
              {canMarkRead && (
                <button
                  onClick={() => onMarkRead?.(src)}
                  className="p-1.5 rounded hover:bg-surface-hover text-text-secondary hover:text-text-primary transition-colors"
                  title="标记为已读"
                >
                  <CheckCheck size={14} />
                </button>
              )}
              {src.url && (
                <a href={src.url} target="_blank" rel="noreferrer" className="p-1.5 rounded hover:bg-surface-hover">
                  <ExternalLink size={14} className="text-text-secondary" />
                </a>
              )}
            </div>
          </div>
        );
      })}
    </div>
  );
}
