import { Loader2 } from 'lucide-react';

interface Props {
  isDiscovering: boolean;
  onDiscover: () => void;
  /** Live progress hint while discovering (streamed from the backend). */
  hint?: string | null;
}

export function AutoDiscoveryPanel({ isDiscovering, onDiscover, hint }: Props) {
  return (
    <div className="p-4 bg-surface border border-surface-hover rounded-xl">
      <div className="flex items-center justify-between">
        <div>
          <p className="text-sm font-medium text-text-primary">自动发现</p>
          <p className="text-xs text-text-secondary mt-1">
            {isDiscovering
              ? hint || '正在搜索 arXiv 与 Crossref…'
              : '从 arXiv 与 Crossref 搜索与课题关键词匹配的最新论文'}
          </p>
        </div>
        <button
          onClick={onDiscover}
          disabled={isDiscovering}
          className="flex items-center gap-2 px-4 py-2 rounded-lg bg-primary text-white text-sm font-medium hover:bg-primary/90 disabled:opacity-50"
        >
          {isDiscovering ? (
            <><Loader2 size={14} className="animate-spin" /> 搜索中...</>
          ) : (
            '开始发现'
          )}
        </button>
      </div>
      {isDiscovering && hint && (
        <div className="mt-2 text-xs text-primary flex items-center gap-1.5">
          <Loader2 size={12} className="animate-spin" />
          {hint} · 结果将逐条显示
        </div>
      )}
    </div>
  );
}
