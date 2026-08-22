import { FlaskConical, Pause, Play, Archive, Trash2, Loader2 } from 'lucide-react';
import type { ResearchTopic } from '@/lib/types';
import { ConfirmButton } from '@/components/ui/ConfirmButton';

interface Props {
  topic: ResearchTopic;
  isActive: boolean;
  onSelect: () => void;
  onDiscover: () => void;
  onTogglePause: () => void;
  onArchive: () => void;
  onDelete: () => void;
  isDiscovering: boolean;
}

const statusBadge: Record<string, { label: string; color: string }> = {
  active: { label: '活跃', color: 'bg-accent/10 text-accent' },
  paused: { label: '暂停', color: 'bg-yellow-500/10 text-yellow-400' },
  completed: { label: '完成', color: 'bg-blue-500/10 text-blue-400' },
  archived: { label: '归档', color: 'bg-text-secondary/10 text-text-secondary' },
};

export function TopicCard({ topic, isActive, onSelect, onDiscover, onTogglePause, onArchive, onDelete, isDiscovering }: Props) {
  const badge = statusBadge[topic.status] || statusBadge.active;
  const keywords: string[] = (() => { try { return JSON.parse(topic.keywords as unknown as string) } catch { return [] } })();

  return (
    <div
      className={`p-4 rounded-xl border cursor-pointer transition-all ${
        isActive ? 'border-primary bg-primary/5' : 'border-surface-hover bg-surface hover:bg-surface-hover'
      }`}
      onClick={onSelect}
    >
      <div className="flex items-start justify-between">
        <div className="flex items-center gap-2">
          <FlaskConical size={18} className="text-primary" />
          <h3 className="font-medium text-text-primary text-sm">{topic.name}</h3>
        </div>
        <span className={`text-xs px-2 py-0.5 rounded-full ${badge.color}`}>{badge.label}</span>
      </div>

      {topic.description && (
        <p className="text-xs text-text-secondary mt-2 line-clamp-2">{topic.description}</p>
      )}

      {keywords.length > 0 && (
        <div className="flex flex-wrap gap-1 mt-2">
          {keywords.map((kw) => (
            <span key={kw} className="text-xs px-1.5 py-0.5 rounded bg-surface-hover text-text-secondary">{kw}</span>
          ))}
        </div>
      )}

      <div className="flex items-center gap-2 mt-3" onClick={(e) => e.stopPropagation()}>
        <button
          onClick={onDiscover}
          disabled={isDiscovering}
          className="text-xs px-2 py-1 rounded bg-primary/10 text-primary hover:bg-primary/20 disabled:opacity-50 flex items-center gap-1"
        >
          {isDiscovering ? <Loader2 size={10} className="animate-spin" /> : null}
          发现
        </button>
        {topic.status === 'active' && (
          <button onClick={onTogglePause} className="p-1 rounded hover:bg-surface-hover"><Pause size={14} /></button>
        )}
        {topic.status === 'paused' && (
          <button onClick={onTogglePause} className="p-1 rounded hover:bg-surface-hover"><Play size={14} /></button>
        )}
        {topic.status !== 'archived' && (
          <button onClick={onArchive} className="p-1 rounded hover:bg-surface-hover"><Archive size={14} /></button>
        )}
        <ConfirmButton icon onConfirm={onDelete} confirmText="确认删除" aria-label="删除课题">
          <Trash2 size={14} />
        </ConfirmButton>
      </div>
    </div>
  );
}
