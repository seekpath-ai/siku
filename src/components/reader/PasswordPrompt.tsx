import { useState } from 'react';
import { Lock, AlertCircle } from 'lucide-react';

interface PasswordPromptProps {
  error?: string | null;
  onSubmit: (password: string) => void;
}

export function PasswordPrompt({ error, onSubmit }: PasswordPromptProps) {
  const [password, setPassword] = useState('');

  const handleSubmit = (e: React.FormEvent) => {
    e.preventDefault();
    onSubmit(password);
  };

  return (
    <div className="absolute inset-0 z-50 flex items-center justify-center bg-background/80 backdrop-blur-sm">
      <form
        onSubmit={handleSubmit}
        className="w-80 rounded-xl border border-surface-hover bg-surface p-5 shadow-xl"
      >
        <div className="mb-4 flex items-center gap-2 text-text-primary">
          <Lock size={18} />
          <h3 className="text-sm font-medium">PDF 受密码保护</h3>
        </div>
        <input
          type="password"
          value={password}
          onChange={(e) => setPassword(e.target.value)}
          placeholder="输入密码..."
          autoFocus
          className="mb-3 w-full rounded-lg border border-surface-hover bg-background px-3 py-2 text-sm text-text-primary outline-none placeholder:text-text-secondary/40 focus:border-primary"
        />
        {error && (
          <div className="mb-3 flex items-start gap-1.5 text-xs text-red-400">
            <AlertCircle size={13} className="mt-0.5 shrink-0" />
            <span>{error}</span>
          </div>
        )}
        <button
          type="submit"
          disabled={!password}
          className="w-full rounded-lg bg-primary px-3 py-2 text-sm font-medium text-primary-foreground hover:bg-primary/90 disabled:cursor-not-allowed disabled:opacity-50"
        >
          解锁
        </button>
      </form>
    </div>
  );
}
