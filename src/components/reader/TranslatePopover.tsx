import { useEffect, useRef, useState } from 'react';
import { Check, Copy, Languages, Loader2, X } from 'lucide-react';
import type { TextSelection } from './PdfViewer';
import { translateTextStream } from '@/lib/tauri';

interface TranslatePopoverProps {
  selection: TextSelection;
  targetLang: string;
  /** Persist the selection as a 智思 snippet together with the translation. */
  onSave: (translation: string) => void;
  onClose: () => void;
}

const POPOVER_WIDTH = 380;
const GAP = 8;

/** Temporary translation of a text selection: an anchored popover streaming
 *  the translation next to the original text. Nothing is persisted unless the
 *  user explicitly saves to 智思 — the popover itself is throwaway: Esc,
 *  outside click, or scrolling the PDF closes it (the backend translation
 *  cache makes a redo of the same selection free). */
export function TranslatePopover({ selection, targetLang, onSave, onClose }: TranslatePopoverProps) {
  const popRef = useRef<HTMLDivElement>(null);
  const bodyRef = useRef<HTMLDivElement>(null);
  const [text, setText] = useState('');
  const [error, setError] = useState<string | null>(null);
  const [done, setDone] = useState(false);
  const [copied, setCopied] = useState(false);
  const [pos, setPos] = useState<{ left: number; top: number } | null>(null);
  const [highlightPath, setHighlightPath] = useState('');

  // Keep the selection visibly highlighted while translating: dismissing the
  // toolbar clears the viewer's selection overlay, so paint our own from the
  // selection's stored per-page ratio rects (same math as the viewer's
  // overlay). Viewport-fixed is fine — the popover closes on scroll.
  useEffect(() => {
    // The viewer's live-selection branch keeps repainting while a native
    // selection exists; drop it so only this highlight remains.
    window.getSelection()?.removeAllRanges();
    const HEIGHT_FACTOR = 0.8; // same shrink the viewer's overlay applies
    // One path for all rects, filled once: font changes produce duplicate
    // boxes for the same span, and a single-path fill can never
    // double-darken the overlaps (stacked translucent divs would).
    let d = '';
    for (const seg of selection.segments ?? []) {
      const wrapper = document.querySelector<HTMLElement>(`[data-page-num="${seg.pageIndex}"]`);
      if (!wrapper) continue;
      const wr = wrapper.getBoundingClientRect();
      const paint = seg.paintRects.length > 0 ? seg.paintRects : seg.rects;
      for (const r of paint) {
        const w = r.widthRatio * wr.width;
        const h = r.heightRatio * wr.height * HEIGHT_FACTOR;
        const x = wr.left + (r.xRatio - r.widthRatio / 2) * wr.width;
        const y = wr.top + r.yRatio * wr.height - h / 2;
        d += `M${x.toFixed(1)} ${y.toFixed(1)}h${w.toFixed(1)}v${h.toFixed(1)}h${(-w).toFixed(1)}Z`;
      }
    }
    setHighlightPath(d);
  }, [selection]);

  // Anchor below the selection, flipping above when the space below is tight.
  // Computed once: the popover closes on scroll instead of following the
  // selection across the PDF viewport.
  useEffect(() => {
    const r = selection.rect;
    const vw = window.innerWidth;
    const vh = window.innerHeight;
    const estH = Math.min(260, Math.round(vh * 0.4));
    let left = r.left + r.width / 2 - POPOVER_WIDTH / 2;
    left = Math.max(8, Math.min(left, vw - POPOVER_WIDTH - 8));
    const belowTop = r.top + r.height + GAP;
    const top =
      belowTop + estH > vh && r.top - GAP > estH ? Math.max(8, r.top - GAP - estH) : belowTop;
    setPos({ left, top });
  }, [selection.rect]);

  // Stream the translation; deltas accumulate locally.
  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        await translateTextStream(selection.text, null, targetLang, (delta) => {
          if (!cancelled) setText((t) => t + delta);
        });
        if (!cancelled) setDone(true);
      } catch (e) {
        if (!cancelled) setError(e instanceof Error ? e.message : String(e));
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [selection.text, targetLang]);

  // Follow the stream to the bottom while it grows.
  useEffect(() => {
    const el = bodyRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [text]);

  // Dismiss: Escape, outside click, or a scroll anywhere outside the popover
  // (the anchor is viewport-fixed, so a scrolling PDF would leave it behind
  // pointing at the wrong text).
  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === 'Escape') onClose();
    };
    const onMouseDown = (e: MouseEvent) => {
      if (popRef.current && !popRef.current.contains(e.target as Node)) onClose();
    };
    const onScroll = (e: Event) => {
      if (popRef.current && e.target instanceof Node && popRef.current.contains(e.target)) return;
      onClose();
    };
    // Delay the outside-click listener: the same mouseup/click that picked
    // "翻译" in the toolbar must not instantly close the popover.
    const id = setTimeout(() => document.addEventListener('mousedown', onMouseDown), 0);
    document.addEventListener('keydown', onKeyDown);
    document.addEventListener('scroll', onScroll, true);
    return () => {
      clearTimeout(id);
      document.removeEventListener('mousedown', onMouseDown);
      document.removeEventListener('keydown', onKeyDown);
      document.removeEventListener('scroll', onScroll, true);
    };
  }, [onClose]);

  const handleCopy = async () => {
    try {
      await navigator.clipboard.writeText(text);
      setCopied(true);
      setTimeout(() => setCopied(false), 1200);
    } catch { /* clipboard unavailable — leave the button state unchanged */ }
  };

  const streaming = !done && !error;

  return (
    <>
      {/* Stand-in selection highlight: keeps the translated passage marked
          while the popover is open (the toolbar dismiss cleared the viewer's
          own overlay). Single path, single fill — same color and overlap
          semantics as the viewer's selection overlay. */}
      {highlightPath && (
        <svg
          className="fixed inset-0 z-30 pointer-events-none"
          style={{ width: '100vw', height: '100vh' }}
        >
          <path
            d={highlightPath}
            style={{ fill: 'color-mix(in srgb, AccentColor, transparent 50%)' }}
          />
        </svg>
      )}
    <div
      ref={popRef}
      className="fixed z-40 w-[380px] max-w-[92vw] flex flex-col rounded-lg bg-surface border border-surface-hover shadow-xl"
      style={{
        left: pos?.left ?? -9999,
        top: pos?.top ?? -9999,
        visibility: pos ? 'visible' : 'hidden',
        maxHeight: '40vh',
      }}
    >
      <div className="flex items-center gap-1.5 px-3 py-2 border-b border-surface-hover">
        <Languages size={13} className="text-primary" />
        <span className="text-xs font-medium text-text-primary">临时翻译</span>
        <button
          onClick={onClose}
          className="ml-auto p-0.5 rounded text-text-secondary hover:text-text-primary hover:bg-surface-hover"
          title="关闭（Esc）"
        >
          <X size={13} />
        </button>
      </div>

      <div ref={bodyRef} className="overflow-y-auto px-3 py-2 min-h-[40px]">
        {error ? (
          <span className="block text-red-400 text-xs break-words" title={error}>
            翻译失败：{error}
          </span>
        ) : !text ? (
          <span className="flex items-center gap-1.5 text-xs text-text-secondary">
            <Loader2 size={11} className="animate-spin" />翻译中…
          </span>
        ) : (
          <span className="block text-xs text-text-primary leading-relaxed whitespace-pre-wrap break-words">
            {text}
            {streaming && <span className="inline-block w-1.5 h-3 bg-primary/70 animate-pulse ml-0.5 align-text-bottom" />}
          </span>
        )}
      </div>

      <div className="flex items-center justify-end gap-1.5 px-3 py-2 border-t border-surface-hover">
        <button
          onClick={handleCopy}
          disabled={!text}
          className="flex items-center gap-1 px-2 py-1 rounded text-xs text-text-secondary hover:bg-surface-hover hover:text-text-primary transition-colors disabled:opacity-40"
          title="复制译文"
        >
          {copied ? <Check size={13} className="text-primary" /> : <Copy size={13} />}
          {copied ? '已复制' : '复制译文'}
        </button>
        <button
          onClick={() => onSave(text)}
          disabled={!done || !text.trim()}
          className="flex items-center gap-1 px-2 py-1 rounded text-xs bg-primary/15 text-primary hover:bg-primary/25 transition-colors disabled:opacity-40"
          title="将选区和译文保存为智思摘录"
        >
          存入智思
        </button>
      </div>
    </div>
    </>
  );
}
