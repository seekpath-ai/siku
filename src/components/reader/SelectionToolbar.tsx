import { useEffect, useRef, useState } from 'react';
import { Copy, StickyNote, Check, Languages } from 'lucide-react';
import type { TextSelection } from './PdfViewer';

interface SelectionToolbarProps {
  selection: TextSelection;
  onCopy: (text: string) => void;
  onSnippet: (sel: TextSelection) => void;
  onTranslate: (sel: TextSelection) => void;
  onDismiss: () => void;
}

const TOOLBAR_HEIGHT = 40; // estimate for flip calculation
const GAP = 8;

export function SelectionToolbar({
  selection,
  onCopy,
  onSnippet,
  onTranslate,
  onDismiss,
}: SelectionToolbarProps) {
  const toolbarRef = useRef<HTMLDivElement>(null);
  const [copied, setCopied] = useState(false);
  const [pos, setPos] = useState({ left: 0, top: 0, flip: false });

  // ── Position toolbar near selection rect ──
  useEffect(() => {
    const r = selection.rect;
    const rBottom = r.top + r.height;
    const vw = window.innerWidth;
    const vh = window.innerHeight;

    // Horizontal: center on selection, clamp to viewport
    const toolbarW = 200; // estimated width
    let left = r.left + r.width / 2 - toolbarW / 2;
    left = Math.max(8, Math.min(left, vw - toolbarW - 8));

    // Vertical: below selection, flip above if not enough space
    const belowTop = rBottom + GAP;
    const aboveBottom = r.top - GAP;
    const flip = belowTop + TOOLBAR_HEIGHT > vh && aboveBottom > rBottom;
    const top = flip ? r.top - TOOLBAR_HEIGHT - GAP : belowTop;

    setPos({ left, top, flip });
  }, [selection.rect]);

  // ── Dismiss on click outside ──
  useEffect(() => {
    const onMouseDown = (e: MouseEvent) => {
      // Ctrl/Cmd+drag starts a multi-range append gesture; the viewer keeps
      // the stored segments and updates the toolbar on mouseup instead.
      if (e.ctrlKey || e.metaKey) return;
      if (toolbarRef.current && !toolbarRef.current.contains(e.target as Node)) {
        onDismiss();
      }
    };
    // Delay listener to avoid the mouseup that created the selection
    const id = setTimeout(() => {
      document.addEventListener('mousedown', onMouseDown);
    }, 0);
    return () => {
      clearTimeout(id);
      document.removeEventListener('mousedown', onMouseDown);
    };
  }, [onDismiss]);

  // ── Dismiss on Escape ──
  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === 'Escape') onDismiss();
    };
    document.addEventListener('keydown', onKeyDown);
    return () => document.removeEventListener('keydown', onKeyDown);
  }, [onDismiss]);

  const handleCopy = async () => {
    try {
      await navigator.clipboard.writeText(selection.text);
      onCopy(selection.text);
      setCopied(true);
      setTimeout(() => {
        onDismiss();
      }, 800);
    } catch {
      // Fallback: use textarea execCommand (rarely needed in Tauri)
      const ta = document.createElement('textarea');
      ta.value = selection.text;
      ta.style.cssText = 'position:fixed;left:-9999px';
      document.body.appendChild(ta);
      ta.select();
      document.execCommand('copy');
      document.body.removeChild(ta);
      setCopied(true);
      setTimeout(() => onDismiss(), 800);
    }
  };

  const handleSnippet = () => {
    onSnippet(selection);
    onDismiss();
  };

  const handleTranslate = () => {
    onTranslate(selection);
    onDismiss();
  };

  const segmentCount = selection.segments?.length ?? 1;

  return (
    <div
      ref={toolbarRef}
      className="fixed z-40 flex items-center gap-1 bg-surface border border-surface-hover rounded-lg shadow-xl px-2 py-1.5"
      style={{
        left: pos.left,
        top: pos.top,
      }}
    >
      {segmentCount > 1 && (
        <>
          <span
            className="px-1.5 py-0.5 rounded text-[10px] font-medium bg-primary/15 text-primary"
            title="多段选区：Ctrl/Cmd+拖动追加，Ctrl/Cmd+点击删除一段"
          >
            {segmentCount} 段
          </span>
          <div className="w-px h-4 bg-surface-hover" />
        </>
      )}
      <button
        onClick={handleCopy}
        className="flex items-center gap-1.5 px-2 py-1 rounded text-xs text-text-secondary hover:bg-surface-hover hover:text-text-primary transition-colors"
        title="复制"
      >
        {copied ? (
          <Check size={14} className="text-primary" />
        ) : (
          <Copy size={14} />
        )}
        {copied ? '已复制' : '复制'}
      </button>

      <div className="w-px h-4 bg-surface-hover" />

      <button
        onClick={handleSnippet}
        className="flex items-center gap-1.5 px-2 py-1 rounded text-xs text-text-secondary hover:bg-surface-hover hover:text-primary transition-colors"
        title="摘录到智思"
      >
        <StickyNote size={14} />
        摘录
      </button>

      <div className="w-px h-4 bg-surface-hover" />

      <button
        onClick={handleTranslate}
        className="flex items-center gap-1.5 px-2 py-1 rounded text-xs text-text-secondary hover:bg-surface-hover hover:text-primary transition-colors"
        title="摘录并翻译到智思"
      >
        <Languages size={14} />
        翻译
      </button>
    </div>
  );
}
