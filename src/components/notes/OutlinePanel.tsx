import type { HeadingItem } from '@/lib/headings';

interface Props {
  items: HeadingItem[];
  /** Index of the heading considered "current" (caret position in edit views,
   *  scroll position in reading view); -1 when above the first heading. */
  activeIdx: number;
  onJump: (item: HeadingItem) => void;
}

/** Right-side outline panel for the note editor (Obsidian-style). Toggled
 *  from the editor's "⋯" menu; lists ATX headings of the current note only
 *  (embedded notes' headings are excluded — the scan runs on raw source). */
export function OutlinePanel({ items, activeIdx, onJump }: Props) {
  return (
    <div className="w-[220px] shrink-0 border-l border-surface-hover flex flex-col min-h-0">
      <div className="px-3 py-2 text-[11px] uppercase tracking-wider text-text-secondary/50 shrink-0">
        大纲
      </div>
      <div className="flex-1 overflow-y-auto pb-2">
        {items.length === 0 ? (
          <div className="px-3 py-1 text-[12px] text-text-secondary/50">当前笔记没有标题</div>
        ) : (
          items.map((it, i) => (
            <button
              key={`${it.offset}-${i}`}
              onClick={() => onJump(it)}
              title={it.text}
              style={{ paddingLeft: `${12 + (it.level - 1) * 12}px` }}
              className={`w-full text-left pr-3 py-1 text-[12px] truncate transition-colors ${
                i === activeIdx
                  ? 'text-primary bg-primary/10'
                  : 'text-text-secondary hover:text-text-primary hover:bg-surface-hover/50'
              }`}
            >
              {it.text}
            </button>
          ))
        )}
      </div>
    </div>
  );
}
