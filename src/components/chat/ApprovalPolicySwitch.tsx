import { useEffect, useRef, useState } from 'react';
import { ShieldCheck, Check } from 'lucide-react';
import type { ApprovalConfig } from '@/lib/types';

/** Approval policy options for the input-area quick switch. */
const APPROVAL_OPTIONS: { value: ApprovalConfig['mode']; label: string; hint: string }[] = [
  { value: 'auto', label: '自动批准', hint: '所有工具调用直接执行' },
  { value: 'auto_expire_time', label: '时间窗口内自动', hint: '批准后一段时间内同类调用免确认' },
  { value: 'auto_by_rules', label: '白名单自动', hint: '仅白名单工具免确认' },
  { value: 'manual', label: '手动审批', hint: '写操作逐条确认，只读免确认' },
  { value: 'manual_all', label: '严格审批', hint: '读写所有调用逐条确认' },
];

interface ApprovalPolicySwitchProps {
  mode: ApprovalConfig['mode'];
  onPick: (mode: ApprovalConfig['mode']) => void;
  disabled?: boolean;
  /** Tooltip suffix, e.g. where the change applies. */
  titleSuffix?: string;
  /** Dense single-line rows (hint moves to the row tooltip) for small
   *  containers like the pet panel, where the full menu is taller than the
   *  panel itself. */
  compact?: boolean;
}

/** Shield button + dropdown for switching the approval policy. Shared by the
 *  main chat input and the pet panel; rendering-agnostic about where the
 *  config is persisted — the parent owns loading/saving and passes the
 *  current mode down. */
export function ApprovalPolicySwitch({ mode, onPick, disabled, titleSuffix, compact }: ApprovalPolicySwitchProps) {
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);

  // Close the dropdown on outside click.
  useEffect(() => {
    if (!open) return;
    const onDown = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) {
        setOpen(false);
      }
    };
    document.addEventListener('mousedown', onDown);
    return () => document.removeEventListener('mousedown', onDown);
  }, [open]);

  return (
    <div className="relative" ref={ref}>
      <button
        type="button"
        disabled={disabled}
        onClick={() => setOpen((o) => !o)}
        className={`w-7 h-7 rounded-md border-0 bg-transparent flex items-center justify-center hover:bg-surface-hover transition-colors disabled:opacity-40 ${
          mode === 'auto'
            ? 'text-text-secondary/50 hover:text-text-primary'
            : mode === 'manual_all'
              ? 'text-amber-400'
              : 'text-primary'
        }`}
        title={`审批策略 · ${APPROVAL_OPTIONS.find((o) => o.value === mode)?.label ?? ''}${titleSuffix ?? ''}`}
      >
        <ShieldCheck size={15} />
      </button>
      {open && (
        <div
          className={`absolute left-0 bottom-full z-50 mb-1 bg-surface border border-surface-hover rounded-lg shadow-xl py-1 overflow-y-auto max-h-[min(320px,50vh)] ${
            compact ? 'w-44' : 'w-60'
          }`}
        >
          {APPROVAL_OPTIONS.map((opt) => (
            <button
              key={opt.value}
              title={compact ? opt.hint : undefined}
              onClick={() => {
                setOpen(false);
                onPick(opt.value);
              }}
              className={`w-full flex items-start gap-2 px-3 text-left hover:bg-surface-hover ${
                compact ? 'py-1 items-center' : 'py-1.5'
              }`}
            >
              <span className="flex-1 min-w-0">
                <span className="block text-[12px] text-text-primary">{opt.label}</span>
                {!compact && (
                  <span className="block text-[10px] text-text-secondary/60">{opt.hint}</span>
                )}
              </span>
              {mode === opt.value && (
                <Check size={13} className="text-primary mt-0.5 shrink-0" />
              )}
            </button>
          ))}
          <div className="px-3 py-1.5 text-[10px] text-text-secondary/50 border-t border-surface-hover mt-1">
            切换从下一条消息开始生效
          </div>
        </div>
      )}
    </div>
  );
}
