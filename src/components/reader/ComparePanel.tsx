import { useEffect, useMemo, useRef } from 'react';
import { Loader2 } from 'lucide-react';
import type { PaperParagraph } from '@/lib/tauri';

interface Props {
  /** null = still loading. */
  paragraphs: PaperParagraph[] | null;
  /** Current PDF page — the pane follows it when continuous sync is off. */
  currentPage: number;
  /** Paragraph highlighted by the last PDF-side click or sync step. */
  activeIndex: number | null;
  onParagraphClick: (index: number, p: PaperParagraph) => void;
  /** True while the PDF side is driving the sync: this pane then follows the
   *  PDF instead of reporting its own position back. */
  followingPdf?: boolean;
  /** Page-granular follow, used when continuous sync is off. */
  followPage?: boolean;
  /** Reports the paragraph at the top of this pane while the user scrolls it,
   *  so the PDF can be aligned to it (one call per animation frame). */
  onVisibleParagraph?: (index: number, p: PaperParagraph) => void;
}

/** Dual-pane text side: paragraphs with page/bbox anchors. Comparison is
 *  paragraph level — line anchors were dropped together with the geometry
 *  pipeline, and the chunker only needs paragraph boundaries. */
export function ComparePanel({
  paragraphs,
  currentPage,
  activeIndex,
  onParagraphClick,
  followingPdf,
  followPage = true,
  onVisibleParagraph,
}: Props) {
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
  // already jumped the PDF to that paragraph, and yanking the pane back to the
  // top of its page would undo it).
  const lastClickAtRef = useRef(0);
  /** Programmatic scrolls must not be mistaken for the user scrolling this
   *  pane — otherwise opening the pane drags the PDF to the page's first
   *  paragraph. */
  const suppressReportUntilRef = useRef(0);
  useEffect(() => {
    if (!followPage) return;
    if (Date.now() - lastClickAtRef.current < 800) return;
    const idx = pageFirstIndex.get(currentPage);
    if (idx == null) return;
    suppressReportUntilRef.current = Date.now() + 400;
    paraRefs.current.get(idx)?.scrollIntoView({ block: 'start' });
  }, [currentPage, pageFirstIndex, followPage]);

  const onVisibleRef = useRef(onVisibleParagraph);
  useEffect(() => { onVisibleRef.current = onVisibleParagraph; }, [onVisibleParagraph]);
  const followingRef = useRef(followingPdf);
  useEffect(() => { followingRef.current = followingPdf; }, [followingPdf]);

  // Report this pane's own scroll position so the PDF can follow it. Only the
  // pane the user is actually scrolling reports (followingPdf is false), and the
  // parent's lock keeps the two directions from pushing each other.
  useEffect(() => {
    const el = scrollRef.current;
    if (!el) return;
    let ticking = false;
    const onScroll = () => {
      if (ticking) return;
      ticking = true;
      requestAnimationFrame(() => {
        ticking = false;
        const cb = onVisibleRef.current;
        if (!cb || followingRef.current) return;
        if (Date.now() < suppressReportUntilRef.current) return;
        const top = el.getBoundingClientRect().top;
        for (const [i, node] of paraRefs.current) {
          if (node.getBoundingClientRect().bottom > top + 4) {
            // Paragraphs without a bbox cannot be located on the PDF; skip to
            // the next one that can.
            for (const j of paraRefs.current.keys()) {
              if (j < i) continue;
              if (paragraphs?.[j]?.bbox) {
                cb(j, paragraphs[j]);
                return;
              }
            }
            return;
          }
        }
      });
    };
    el.addEventListener('scroll', onScroll, { passive: true });
    return () => el.removeEventListener('scroll', onScroll);
  }, [paragraphs]);

  // Reveal the active paragraph. While the PDF is driving, align it to the top
  // instantly — smooth centering lags behind a fast scroll and can leave the
  // block off-screen, which is the whole point of the sync.
  useEffect(() => {
    if (activeIndex == null) return;
    const node = paraRefs.current.get(activeIndex);
    const el = scrollRef.current;
    if (!node) return;
    suppressReportUntilRef.current = Date.now() + (followingPdf ? 200 : 800);
    if (followingPdf && el) {
      el.scrollTop += node.getBoundingClientRect().top - el.getBoundingClientRect().top - 8;
      return;
    }
    node.scrollIntoView({ block: 'center', behavior: 'smooth' });
  }, [activeIndex, followingPdf]);

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
