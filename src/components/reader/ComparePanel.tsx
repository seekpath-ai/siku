import { useEffect, useMemo, useRef } from 'react';
import { Languages, Loader2 } from 'lucide-react';
import type { PaperParagraph } from '@/lib/tauri';

export type CompareDisplayMode = 'orig' | 'bilingual' | 'trans';

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
  /** 显示模式：原文 / 双语 / 译文。缺省 'orig' 时保持原行为。 */
  displayMode?: CompareDisplayMode;
  /** 段落译文（index → 文本）。 */
  translations?: Record<number, string>;
  /** 翻译失败的段落（index → 错误信息）。 */
  translationErrors?: Record<number, string>;
  /** 重试某一段的翻译。 */
  onRetryParagraph?: (index: number) => void;
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
  displayMode = 'orig',
  translations = {},
  translationErrors = {},
  onRetryParagraph,
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
              {displayMode !== 'trans' && <div>{p.text}</div>}
              {displayMode !== 'orig' && (
                translations[i] != null ? (
                  <div
                    className={
                      displayMode === 'bilingual'
                        ? 'mt-1.5 pt-1.5 border-t border-surface-hover/50 text-text-secondary'
                        : ''
                    }
                  >
                    {translations[i]}
                  </div>
                ) : translationErrors[i] != null ? (
                  <div className={displayMode === 'bilingual' ? 'mt-1.5 pt-1.5 border-t border-surface-hover/50' : ''}>
                    <button
                      onClick={(e) => {
                        e.stopPropagation();
                        onRetryParagraph?.(i);
                      }}
                      className="inline-flex items-center gap-1 text-[11px] text-red-400/90 hover:text-red-300"
                      title={translationErrors[i]}
                    >
                      <Languages size={11} />
                      翻译失败，点击重试
                    </button>
                    {displayMode === 'trans' && <div className="mt-1 text-text-primary/50">{p.text}</div>}
                  </div>
                ) : (
                  // 尚未翻译：译文模式下保留原文（弱化显示），双语模式下显示占位。
                  displayMode === 'bilingual' ? (
                    <div className="mt-1.5 pt-1.5 border-t border-surface-hover/50 text-text-secondary/40 text-[11px]">
                      未翻译
                    </div>
                  ) : (
                    <div className="text-text-primary/50">{p.text}</div>
                  )
                )
              )}
            </div>
          </div>
        );
      })}
    </div>
  );
}
