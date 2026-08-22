import { useEffect, useRef, useState } from 'react';
import { Palette, Plus, X, Check } from 'lucide-react';
import { PRESET_THEMES, type ReaderTheme } from './themes';

interface ThemePickerProps {
  themeId: string;
  customThemes: ReaderTheme[];
  onSelect: (id: string) => void;
  onAddCustom: (draft: { name: string; background: string; foreground: string }) => void;
  onDeleteCustom: (id: string) => void;
}

/** Toolbar button + dropdown for picking a reader page theme, with a small
 *  inline form for creating custom background/foreground color pairs. */
export function ThemePicker({ themeId, customThemes, onSelect, onAddCustom, onDeleteCustom }: ThemePickerProps) {
  const [open, setOpen] = useState(false);
  const [adding, setAdding] = useState(false);
  const [name, setName] = useState('');
  const [background, setBackground] = useState('#2E3440');
  const [foreground, setForeground] = useState('#D8DEE9');
  const rootRef = useRef<HTMLDivElement>(null);

  // Close when clicking outside.
  useEffect(() => {
    if (!open) return;
    const onClick = (e: MouseEvent) => {
      if (rootRef.current && !rootRef.current.contains(e.target as Node)) {
        setOpen(false);
        setAdding(false);
      }
    };
    document.addEventListener('click', onClick, true);
    return () => document.removeEventListener('click', onClick, true);
  }, [open]);

  const allThemes = [...PRESET_THEMES, ...customThemes];

  const handleSave = () => {
    const trimmed = name.trim();
    if (!trimmed) return;
    onAddCustom({ name: trimmed, background, foreground });
    setName('');
    setAdding(false);
  };

  const swatch = (t: ReaderTheme) => {
    const selected = t.id === themeId;
    const isOriginal = !t.background;
    return (
      <div key={t.id} className="relative group">
        <button
          onClick={() => { onSelect(t.id); setOpen(false); }}
          className={`w-16 h-11 rounded-md border flex flex-col items-center justify-center gap-0.5 transition-shadow ${
            selected ? 'ring-2 ring-primary border-primary' : 'border-surface-hover hover:ring-1 hover:ring-primary/50'
          }`}
          style={{
            background: isOriginal ? '#FFFFFF' : t.background,
            color: isOriginal ? '#24292F' : t.foreground,
          }}
          title={t.name}
        >
          <span className="text-[11px] font-medium leading-none">Aa</span>
          <span className="text-[10px] leading-none">{t.name}</span>
          {selected && <Check size={10} className="absolute bottom-1 right-1" />}
        </button>
        {t.custom && (
          <button
            onClick={(e) => { e.stopPropagation(); onDeleteCustom(t.id); }}
            className="absolute -top-1.5 -right-1.5 hidden group-hover:flex w-4 h-4 items-center justify-center rounded-full bg-surface border border-surface-hover text-text-secondary hover:text-red-400"
            title="删除主题"
          >
            <X size={10} />
          </button>
        )}
      </div>
    );
  };

  return (
    <div ref={rootRef} className="relative">
      <button
        onClick={() => { setOpen((v) => !v); setAdding(false); }}
        className={`p-1 rounded transition-colors ${
          open || themeId !== 'original' ? 'bg-primary/10 text-primary' : 'text-text-secondary hover:bg-surface-hover'
        }`}
        title="主题"
        aria-label="主题"
      >
        <Palette size={14} />
      </button>
      {open && (
        <div className="absolute right-0 top-full mt-1 z-40 bg-surface border border-surface-hover rounded-lg shadow-xl p-2 w-[232px]">
          {!adding ? (
            <>
              <div className="text-[10px] text-text-secondary/60 px-1 pb-1.5">主题</div>
              <div className="grid grid-cols-3 gap-2">
                {allThemes.map(swatch)}
                <button
                  onClick={() => setAdding(true)}
                  className="w-16 h-11 rounded-md border border-dashed border-surface-hover flex items-center justify-center text-text-secondary hover:text-text-primary hover:border-primary/50 transition-colors"
                  title="添加自定义主题"
                >
                  <Plus size={16} />
                </button>
              </div>
            </>
          ) : (
            <div className="px-1 pb-1">
              <div className="text-[10px] text-text-secondary/60 pb-2">自定义主题</div>
              <label className="flex items-center gap-2 pb-2 text-xs text-text-secondary">
                <span className="w-14 shrink-0">主题名称</span>
                <input
                  value={name}
                  onChange={(e) => setName(e.target.value)}
                  className="flex-1 min-w-0 px-1.5 py-1 rounded border border-surface-hover bg-transparent text-text-primary text-xs outline-none focus:border-primary"
                  placeholder="我的主题"
                  autoFocus
                />
              </label>
              <label className="flex items-center gap-2 pb-2 text-xs text-text-secondary">
                <span className="w-14 shrink-0">背景色</span>
                <input
                  type="color"
                  value={background}
                  onChange={(e) => setBackground(e.target.value)}
                  className="w-7 h-6 p-0 border border-surface-hover rounded bg-transparent cursor-pointer"
                />
                <span className="text-[10px] text-text-secondary/60 tabular-nums">{background.toUpperCase()}</span>
              </label>
              <label className="flex items-center gap-2 pb-3 text-xs text-text-secondary">
                <span className="w-14 shrink-0">前景色</span>
                <input
                  type="color"
                  value={foreground}
                  onChange={(e) => setForeground(e.target.value)}
                  className="w-7 h-6 p-0 border border-surface-hover rounded bg-transparent cursor-pointer"
                />
                <span className="text-[10px] text-text-secondary/60 tabular-nums">{foreground.toUpperCase()}</span>
              </label>
              <div className="flex justify-end gap-2">
                <button
                  onClick={() => setAdding(false)}
                  className="px-2.5 py-1 rounded text-xs text-text-secondary hover:bg-surface-hover transition-colors"
                >
                  取消
                </button>
                <button
                  onClick={handleSave}
                  disabled={!name.trim()}
                  className="px-2.5 py-1 rounded text-xs bg-primary text-white hover:bg-primary/90 transition-colors disabled:opacity-40"
                >
                  保存
                </button>
              </div>
            </div>
          )}
        </div>
      )}
    </div>
  );
}
