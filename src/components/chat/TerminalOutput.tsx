import { Loader2 } from 'lucide-react';

interface TerminalOutputProps {
  output: string;
  status?: 'running' | 'completed' | 'error' | 'timeout';
  command?: string;
}

export function TerminalOutput({ output, status = 'completed', command }: TerminalOutputProps) {
  const isRunning = status === 'running';

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
      </div>
      <div className="px-3 py-2.5 font-mono text-[12px] leading-relaxed text-codex-secondary whitespace-pre-wrap">
        {status === 'error' || status === 'timeout' ? (
          <span className="text-codex-danger">{output}</span>
        ) : (
          output.split('\n').map((line, i) => (
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
      </div>
    </div>
  );
}
