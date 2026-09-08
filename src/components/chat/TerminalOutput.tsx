import { Loader2 } from 'lucide-react';

interface TerminalOutputProps {
  output: string;
  status?: 'running' | 'completed' | 'error' | 'timeout';
  command?: string;
  /** Collapsed preview: only the output tail is shown behind a fade mask. */
  collapsed?: boolean;
}

/** Lines kept in the collapsed (unexpanded) preview. */
const COLLAPSED_TAIL_LINES = 5;

export function TerminalOutput({ output, status = 'completed', command, collapsed }: TerminalOutputProps) {
  const isRunning = status === 'running';
  const lines = output.split('\n');
  const truncated = collapsed && lines.length > COLLAPSED_TAIL_LINES;
  const shown = truncated ? lines.slice(-COLLAPSED_TAIL_LINES) : lines;

  return (
    <div className="mt-[-1px] bg-codex-code border-t border-codex-border">
      <div className="flex items-center gap-2 px-3 py-1.5 text-[11px] text-codex-muted">
        {isRunning ? (
          <Loader2 size={12} className="animate-spin text-codex-accent" />
        ) : (
          <span
            className={`w-1.5 h-1.5 rounded-full ${
              status === 'error' || status === 'timeout' ? 'bg-codex-danger' : 'bg-codex-accent'
            }`}
          />
        )}
        <span>{command ? `> ${command}` : '命令输出'}</span>
        {truncated && <span className="ml-auto">仅显示末尾 {COLLAPSED_TAIL_LINES} 行，展开查看全部</span>}
      </div>
      <div
        className={`relative px-3 py-2.5 font-mono text-[12px] leading-relaxed text-codex-secondary whitespace-pre-wrap ${
          collapsed ? 'overflow-hidden' : 'max-h-[400px] overflow-y-auto'
        }`}
      >
        {status === 'error' || status === 'timeout' ? (
          <span className="text-codex-danger">{collapsed ? shown.join('\n') : output}</span>
        ) : (
          shown.map((line, i) => (
            <div key={i}>
              {line.startsWith('$') ? (
                <span className="text-codex-muted">{line}</span>
              ) : line.startsWith('✓') || line.startsWith('✔') ? (
                <span className="text-codex-accent">{line}</span>
              ) : line.startsWith('✗') || line.startsWith('✖') ? (
                <span className="text-codex-danger">{line}</span>
              ) : (
                line
              )}
            </div>
          ))
        )}
        {truncated && (
          <div className="pointer-events-none absolute inset-x-0 top-0 h-10 bg-gradient-to-b from-codex-code via-codex-code/80 to-transparent" />
        )}
      </div>
    </div>
  );
}
