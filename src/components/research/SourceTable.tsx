import { useState } from 'react';
import { Download, FileCheck, Eye, ExternalLink, Loader2, CheckCheck, Clock, ChevronDown, ChevronUp, Star } from 'lucide-react';
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

/** Below this LLM relevance score a source folds into the "低相关" group. */
const LOW_RELEVANCE = 6;

function scoreChip(src: ResearchSource) {
  if (src.relevance_score == null) return null;
  const s = src.relevance_score;
  const color =
    s >= 8 ? 'bg-accent/15 text-accent' : s >= LOW_RELEVANCE ? 'bg-primary/10 text-primary' : 'bg-surface-hover text-text-secondary';
  return (
    <span
      className={`flex items-center gap-0.5 text-[11px] px-1.5 py-0.5 rounded ${color}`}
      title={src.relevance_reason ? `相关性 ${s}/10：${src.relevance_reason}` : `相关性 ${s}/10`}
    >
      <Star size={10} />
      {s}
    </span>
  );
}

function SourceRow({ src, importingId, onImport, onMarkRead }: { src: ResearchSource } & Omit<Props, 'sources'>) {
  const importing = importingId === src.id;
  const canImport = (src.status === 'discovered' || src.status === 'downloaded') && !!onImport;
  const canMarkRead = src.status === 'imported' && !!onMarkRead;
  return (
    <div className="flex items-center gap-3 p-3 bg-surface border border-surface-hover rounded-lg text-sm">
      <div className="flex-1 min-w-0">
        <div className="flex items-center gap-2">
          <span className="text-xs px-1.5 py-0.5 rounded bg-surface-hover text-text-secondary">{src.source_type}</span>
          <span className="flex items-center gap-1 text-xs text-text-secondary">
            {statusIcons[src.status]}{statusLabel[src.status]}
          </span>
          {scoreChip(src)}
          {src.discovered_at && (
            <span className="flex items-center gap-1 text-[11px] text-text-secondary/50">
              <Clock size={10} />
              {isoToDisplay(src.discovered_at)}
            </span>
          )}
        </div>
        <p className="font-medium text-text-primary mt-1 truncate">{src.title || 'Untitled'}</p>
        {src.authors && <p className="text-xs text-text-secondary mt-0.5 truncate">{src.authors}</p>}
        {src.relevance_reason && (
          <p className="text-[11px] text-text-secondary/70 mt-0.5 truncate" title={src.relevance_reason}>
            {src.relevance_reason}
          </p>
        )}
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
}

export function SourceTable({ sources, importingId, onImport, onMarkRead }: Props) {
  const [showLow, setShowLow] = useState(false);

  if (sources.length === 0) {
    return <p className="text-sm text-text-secondary py-4">暂无发现的论文。点击"开始发现"搜索前沿研究。</p>;
  }

  // Low-scoring sources fold away; unscored (NULL) and high-scoring stay in
  // the main list. Order within each group is unchanged (discovery order).
  const main = sources.filter((s) => s.relevance_score == null || s.relevance_score >= LOW_RELEVANCE);
  const low = sources.filter((s) => s.relevance_score != null && s.relevance_score < LOW_RELEVANCE);

  return (
    <div className="space-y-1">
      {main.map((src) => (
        <SourceRow key={src.id} src={src} importingId={importingId} onImport={onImport} onMarkRead={onMarkRead} />
      ))}
      {low.length > 0 && (
        <div className="pt-1">
          <button
            onClick={() => setShowLow((v) => !v)}
            className="flex items-center gap-1 text-xs text-text-secondary/70 hover:text-text-primary px-1 py-1"
          >
            {showLow ? <ChevronUp size={12} /> : <ChevronDown size={12} />}
            低相关（{low.length}）
          </button>
          {showLow && (
            <div className="space-y-1 opacity-75 mt-1">
              {low.map((src) => (
                <SourceRow key={src.id} src={src} importingId={importingId} onImport={onImport} onMarkRead={onMarkRead} />
              ))}
            </div>
          )}
        </div>
      )}
    </div>
  );
}
