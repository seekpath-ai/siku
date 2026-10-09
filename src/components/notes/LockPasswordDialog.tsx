import { useEffect, useRef, useState } from 'react';
import { Lock, LockOpen, X, Eye, EyeOff, Loader2 } from 'lucide-react';
import { vaultSetLockPassword, vaultVerifyLockPassword } from '@/lib/tauri';
import { useNoteLockStore } from '@/stores/noteLockStore';
import type { Note } from '@/lib/types';

interface DialogProps {
  /** setup = first-time global lock password (two inputs); verify = unlock. */
  mode: 'setup' | 'verify';
  /** Dialog heading; defaults per mode. */
  title?: string;
  /** Called with the verified/new password after success. */
  onSuccess: (password: string) => void;
  onClose: () => void;
}

function PasswordInput({
  value,
  onChange,
  placeholder,
  autoFocus,
}: {
  value: string;
  onChange: (v: string) => void;
  placeholder: string;
  autoFocus?: boolean;
}) {
  const [visible, setVisible] = useState(false);
  return (
    <div className="relative">
      <input
        type={visible ? 'text' : 'password'}
        value={value}
        onChange={(e) => onChange(e.target.value)}
        placeholder={placeholder}
        autoFocus={autoFocus}
        className="w-full h-9 bg-background text-text-primary text-[13px] pl-3 pr-9 rounded border border-surface-hover focus:border-primary/50 focus:outline-none placeholder:text-text-secondary/40"
      />
      <button
        type="button"
        onClick={() => setVisible((v) => !v)}
        className="absolute right-2 top-1/2 -translate-y-1/2 text-text-secondary/60 hover:text-text-primary transition-colors"
        title={visible ? '隐藏密码' : '显示密码'}
      >
        {visible ? <EyeOff size={14} /> : <Eye size={14} />}
      </button>
    </div>
  );
}

/** Modal for setting (first time) or verifying the vault lock password. */
export function LockPasswordDialog({ mode, title, onSuccess, onClose }: DialogProps) {
  const [password, setPassword] = useState('');
  const [confirm, setConfirm] = useState('');
  const [error, setError] = useState('');
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    const onDown = (e: KeyboardEvent) => {
      if (e.key === 'Escape') onClose();
    };
    window.addEventListener('keydown', onDown);
    return () => window.removeEventListener('keydown', onDown);
  }, [onClose]);

  const submit = async () => {
    if (busy) return;
    setError('');
    if (mode === 'setup') {
      if (!password) {
        setError('密码不能为空');
        return;
      }
      if (password !== confirm) {
        setError('两次输入的密码不一致');
        return;
      }
      setBusy(true);
      try {
        await vaultSetLockPassword(password);
        onSuccess(password);
      } catch (err) {
        setError(String(err));
      } finally {
        setBusy(false);
      }
      return;
    }
    if (!password) return;
    setBusy(true);
    try {
      const ok = await vaultVerifyLockPassword(password);
      if (ok) onSuccess(password);
      else setError('密码错误');
    } catch (err) {
      setError(String(err));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="fixed inset-0 z-[300] flex items-center justify-center">
      <div className="absolute inset-0 bg-black/50 backdrop-blur-sm" onClick={onClose} />
      <div className="relative w-[320px] bg-surface border border-surface-hover rounded-xl shadow-2xl overflow-hidden">
        <div className="flex items-center justify-between px-4 py-3 border-b border-surface-hover">
          <span className="flex items-center gap-1.5 text-sm font-medium text-text-primary">
            <Lock size={13} className="text-text-secondary" />
            {title ?? (mode === 'setup' ? '设置锁定密码' : '输入锁定密码')}
          </span>
          <button
            onClick={onClose}
            className="p-1 rounded text-text-secondary/60 hover:text-text-primary hover:bg-surface-hover transition-colors"
            aria-label="关闭"
          >
            <X size={14} />
          </button>
        </div>
        <form
          className="p-4 flex flex-col gap-2.5"
          onSubmit={(e) => {
            e.preventDefault();
            void submit();
          }}
        >
          {mode === 'setup' && (
            <p className="text-[11px] text-text-secondary/70 leading-relaxed">
              首次使用笔记锁定，请先设置密码。该密码用于解锁所有锁定的笔记和文件夹，请牢记。
            </p>
          )}
          <PasswordInput value={password} onChange={setPassword} placeholder="密码" autoFocus />
          {mode === 'setup' && (
            <PasswordInput value={confirm} onChange={setConfirm} placeholder="确认密码" />
          )}
          {error && <p className="text-[11px] text-red-400">{error}</p>}
          <button
            type="submit"
            disabled={busy}
            className="mt-1 h-8 rounded bg-primary/15 text-primary text-xs font-medium hover:bg-primary/25 transition-colors disabled:opacity-50 flex items-center justify-center gap-1.5"
          >
            {busy && <Loader2 size={12} className="animate-spin" />}
            {mode === 'setup' ? '设置并继续' : '解锁'}
          </button>
        </form>
      </div>
    </div>
  );
}

interface CoverProps {
  note: Note;
  /** Lock-root ids gating this note (itself + not-yet-unlocked locked
   *  ancestors). One successful verification session-unlocks all of them, so
   *  opening a note inside a locked folder releases the folder's subtree. */
  gateIds: string[];
  /** Extra callback after a successful unlock (store is already updated). */
  onUnlocked?: () => void;
}

/** Full-area cover for a locked note: hides the whole content region behind a
 *  centered password prompt. No cached/silent unlock — the password must be
 *  typed every time. */
export function LockedNoteCover({ note, gateIds, onUnlocked }: CoverProps) {
  const [password, setPassword] = useState('');
  const [error, setError] = useState('');
  const [busy, setBusy] = useState(false);
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    inputRef.current?.focus();
  }, []);

  const submit = async () => {
    if (!password || busy) return;
    setBusy(true);
    setError('');
    try {
      const ok = await vaultVerifyLockPassword(password);
      if (ok) {
        useNoteLockStore
          .getState()
          .unlockMany(note.vault_id, gateIds.length > 0 ? gateIds : [note.id]);
        onUnlocked?.();
      } else {
        setError('密码错误');
      }
    } catch (err) {
      setError(String(err));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="h-full w-full flex flex-col items-center justify-center gap-3 bg-background select-none">
      <div className="w-14 h-14 rounded-2xl bg-surface-hover/60 flex items-center justify-center">
        <Lock size={26} className="text-text-secondary" />
      </div>
      <p className="text-sm text-text-primary">此笔记已锁定</p>
      <form
        className="flex flex-col items-center gap-2 w-[240px]"
        onSubmit={(e) => {
          e.preventDefault();
          void submit();
        }}
      >
        <input
          ref={inputRef}
          type="password"
          value={password}
          onChange={(e) => setPassword(e.target.value)}
          placeholder="输入密码解锁"
          className="w-full h-9 bg-surface text-text-primary text-[13px] px-3 rounded border border-surface-hover focus:border-primary/50 focus:outline-none placeholder:text-text-secondary/40 text-center"
        />
        {error && <p className="text-[11px] text-red-400">{error}</p>}
        <button
          type="submit"
          disabled={busy || !password}
          className="w-full h-8 rounded bg-primary/15 text-primary text-xs font-medium hover:bg-primary/25 transition-colors disabled:opacity-50 flex items-center justify-center gap-1.5"
        >
          {busy ? <Loader2 size={12} className="animate-spin" /> : <LockOpen size={12} />}
          解锁
        </button>
      </form>
    </div>
  );
}
