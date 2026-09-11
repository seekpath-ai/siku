import { useEffect, useMemo, useRef } from 'react';
import { Loader2 } from 'lucide-react';
import type { PaperParagraph } from '@/lib/tauri';

interface Props {
  /** null = still loading. */
  paragraphs: PaperParagraph[] | null;
  /** Current PDF page — the pane follows it (scrolls to that page's first
   *  paragraph). */
  currentPage: number;
  /** Paragraph hit by the last PDF-side click: flashed and scrolled into
   *  view. Index into the paragraphs array. */
  activeIndex: number | null;
  onParagraphClick: (index: number, p: PaperParagraph) => void;
}

/** Dual-pane Markdown side: paragraphs extracted with page/bbox anchors.
 *  Click a paragraph to jump+highlight on the PDF; clicking the PDF (handled
 *  by the parent) flashes the matching paragraph here. */
export function ComparePanel({ paragraphs, currentPage, activeIndex, onParagraphClick }: Props) {
  const scrollRef = useRef<HTMLDivElement>(null);
  const paraRefs = useRef<Map<number, HTMLDivElement>>(new Map());

  // First paragraph index of each page, for page-follow scrolling.
  const pageFirstIndex = useMemo(() => {
    const m = new Map<number, number>();
    paragraphs?.forEach((p, i) => {
      if (!m.has(p.page)) m.set(p.page, i);
    });
    return m;
  }, [paragraphs]);

  // Follow the PDF page — but not right after a paragraph click (the click
  // already jumped the PDF to the paragraph's page and we must not yank the
  // pane back to the top of that page).
  const lastClickAtRef = useRef(0);
  useEffect(() => {
    if (Date.now() - lastClickAtRef.current < 800) return;
    const idx = pageFirstIndex.get(currentPage);
    if (idx == null) return;
    paraRefs.current.get(idx)?.scrollIntoView({ block: 'start' });
  }, [currentPage, pageFirstIndex]);

  // Flash + reveal the paragraph hit by a PDF-side click.
  useEffect(() => {
    if (activeIndex == null) return;
    paraRefs.current.get(activeIndex)?.scrollIntoView({ block: 'center', behavior: 'smooth' });
  }, [activeIndex]);

  if (!paragraphs) {
    return (
      <div className="flex flex-col items-center justify-center h-full gap-2 text-text-secondary">
        <Loader2 size={18} className="animate-spin" />
        <span className="text-xs">正在解析段落…</span>
      </div>
    );
  }

  let lastPage = 0;
  return (
    <div ref={scrollRef} className="h-full overflow-y-auto px-3 py-2">
      {paragraphs.map((p, i) => {
        const pageBreak = p.page !== lastPage;
        lastPage = p.page;
        return (
          <div key={i}>
            {pageBreak && (
              <div className="sticky top-0 z-10 -mx-3 px-3 py-1 bg-surface/95 backdrop-blur text-[10px] text-text-secondary/60 border-b border-surface-hover/50">
                第 {p.page} 页
              </div>
            )}
            <div
              ref={(el) => {
                if (el) paraRefs.current.set(i, el);
                else paraRefs.current.delete(i);
              }}
              onClick={() => {
                lastClickAtRef.current = Date.now();
                onParagraphClick(i, p);
              }}
              className={`my-1.5 px-2 py-1.5 rounded text-[13px] leading-relaxed cursor-pointer transition-colors whitespace-pre-wrap [overflow-wrap:anywhere] ${
                activeIndex === i
                  ? 'bg-primary/15 text-text-primary'
                  : 'text-text-primary/85 hover:bg-surface-hover'
              }`}
            >
              {p.text}
            </div>
          </div>
        );
      })}
    </div>
  );
}
