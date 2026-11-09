import { CheckCircle2, Info, AlertTriangle, XCircle, X } from 'lucide-react';
import { useToasts, dismissToast, type ToastKind } from '@/lib/notify';
import { Z_TIERS } from './Modal';

const KIND_STYLE: Record<ToastKind, { icon: typeof Info; classes: string }> = {
  success: { icon: CheckCircle2, classes: 'bg-emerald-500/10 border-emerald-500/30 text-emerald-400' },
  info: { icon: Info, classes: 'bg-blue-500/10 border-blue-500/30 text-blue-400' },
  warning: { icon: AlertTriangle, classes: 'bg-yellow-500/10 border-yellow-500/30 text-yellow-400' },
  error: { icon: XCircle, classes: 'bg-red-500/10 border-red-500/30 text-red-400' },
};

/** 主窗口 toast 渲染端（src/lib/notify.ts 的配对组件，挂在 AppShell）。 */
export function ToastHost() {
  const toasts = useToasts();
  if (toasts.length === 0) return null;
  return (
    <div
      className="fixed bottom-4 right-4 flex flex-col gap-2 items-end pointer-events-none"
      style={{ zIndex: Z_TIERS.onboarding }}
    >
      {toasts.map((t) => {
        const { icon: Icon, classes } = KIND_STYLE[t.kind];
        return (
          <div
            key={t.id}
            className={`pointer-events-auto flex items-center gap-2 max-w-[360px] px-3 py-2 border rounded-lg text-xs shadow-lg bg-surface ${classes}`}
          >
            <Icon size={14} className="shrink-0" />
            <span className="break-words text-text-primary">{t.text}</span>
            <button
              onClick={() => dismissToast(t.id)}
              className="shrink-0 ml-1 text-text-secondary/60 hover:text-text-primary"
              aria-label="关闭通知"
            >
              <X size={12} />
            </button>
          </div>
        );
      })}
    </div>
  );
}
