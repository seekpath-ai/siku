import { useEffect, useState } from 'react';
import { Loader2, Check } from 'lucide-react';

interface SaveButtonProps {
  saving: boolean;
  saved: boolean;
  onClick: () => void;
  /** Button label; defaults to 保存. */
  label?: string;
  disabled?: boolean;
}

/**
 * Unified settings save button:
 * - spinner while saving, idle label otherwise
 * - "已保存" success feedback that auto-hides after 2s
 * - right-aligned, primary style consistent across all settings pages
 */
export function SaveButton({ saving, saved, onClick, label = '保存', disabled }: SaveButtonProps) {
  const [showSaved, setShowSaved] = useState(false);

  useEffect(() => {
    if (!saved) return;
    setShowSaved(true);
    const timer = setTimeout(() => setShowSaved(false), 2000);
    return () => clearTimeout(timer);
  }, [saved]);

  return (
    <div className="flex items-center justify-end gap-3">
      <button
        type="button"
        onClick={onClick}
        disabled={disabled || saving}
        className="flex items-center gap-1.5 px-4 py-2 rounded-lg bg-primary text-white text-sm font-medium hover:bg-primary/90 disabled:opacity-50 disabled:pointer-events-none transition-colors"
      >
        {saving && <Loader2 size={14} className="animate-spin" />}
        {label}
      </button>
      {showSaved && (
        <span className="flex items-center gap-1 text-xs text-accent">
          <Check size={12} /> 已保存
        </span>
      )}
    </div>
  );
}
