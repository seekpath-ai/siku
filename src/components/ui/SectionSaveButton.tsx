import type { ReactNode } from 'react';
import { Check, Loader2 } from 'lucide-react';

interface SectionSaveButtonProps {
  saving: boolean;
  saved: boolean;
  onClick: () => void;
  /** Extra buttons rendered left of the save button (e.g. 恢复默认). */
  children?: ReactNode;
  disabled?: boolean;
  label?: string;
  className?: string;
}

/** Section-level save button, pet-page style: a small soft-primary button in
 *  a right-aligned row, with the state inside the button
 *  (保存中... → 已保存 → 保存). The canonical save affordance for settings
 *  sections — the solid SaveButton stays for whole-form pages. */
export function SectionSaveButton({
  saving,
  saved,
  onClick,
  children,
  disabled,
  label = '保存',
  className,
}: SectionSaveButtonProps) {
  return (
    <div className={`flex items-center justify-end gap-2 ${className ?? ''}`}>
      {children}
      <button
        type="button"
        onClick={onClick}
        disabled={disabled || saving}
        className="flex items-center gap-1.5 px-3 py-1.5 rounded-lg bg-primary/10 text-primary text-xs hover:bg-primary/20 transition-colors disabled:opacity-50"
      >
        {saving ? (
          <>
            <Loader2 size={11} className="animate-spin" />
            保存中...
          </>
        ) : saved ? (
          <>
            <Check size={11} />
            已保存
          </>
        ) : (
          label
        )}
      </button>
    </div>
  );
}
