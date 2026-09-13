import { useEffect, useMemo, useRef } from 'react';
import { Loader2 } from 'lucide-react';
import type { PaperParagraph, PaperLineAnchor } from '@/lib/tauri';

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
  /** True while the PDF side is driving the sync: this pane then follows the
   *  PDF instead of reporting its own position back. */
  followingPdf?: boolean;
  /** Page-granular follow, used when continuous sync is off. With sync on the
   *  anchor follow supersedes it and the two would fight over the scroll. */
  followPage?: boolean;
  /** Reports the paragraph at the top of this pane while the user scrolls it,
   *  so the PDF can be aligned to it. Throttled to one call per animation frame. */
  onVisibleParagraph?: (index: number, p: PaperParagraph) => void;
  /** Line-level variant, preferred when paragraphs carry line anchors. */
  onVisibleLine?: (
    index: number,
    lineIndex: number,
    p: PaperParagraph,
    line: PaperLineAnchor
  ) => void;
  /** Line the PDF side is pointing at, highlighted and kept in view. */
  activeLine?: { paragraph: number; line: number } | null;
}

interface LineNode {
  paragraph: number;
  line: number;
  el: HTMLDivElement;
}

/** First rendered line whose bottom edge is below `y` — the line at the top of
 *  the viewport. Line rects increase monotonically down the pane, so a binary
 *  search is enough even for a book-length document. */
function firstLineBelow(nodes: LineNode[], y: number): number {
  let lo = 0;
  let hi = nodes.length - 1;
  let found = -1;
  while (lo <= hi) {
    const mid = (lo + hi) >> 1;
    const rect = nodes[mid].el.getBoundingClientRect();
    if (rect.bottom > y + 4) {
      found = mid;
      hi = mid - 1;
    } else {
      lo = mid + 1;
    }
  }
  return found;
}

/** Dual-pane Markdown side: paragraphs extracted with page/bbox anchors, one
 *  element per line. Click a paragraph to jump+highlight on the PDF; clicking
 *  the PDF (handled by the parent) reveals the matching line here. */
export function ComparePanel({
  paragraphs,
  currentPage,
  activeIndex,
  onParagraphClick,
  followingPdf,
  followPage = true,
  onVisibleParagraph,
  onVisibleLine,
  activeLine,
}: Props) {
  const scrollRef = useRef<HTMLDivElement>(null);
  const paraRefs = useRef<Map<number, HTMLDivElement>>(new Map());
  const lineRefs = useRef<Map<string, HTMLDivElement>>(new Map());
  /** Line nodes in reading order, rebuilt after every render (refs are
   *  registered during render, so the order comes from the "para:line" keys). */
  const lineNodesRef = useRef<LineNode[]>([]);

  /** Character ranges of each paragraph's lines, from the backend's per-line
   *  lengths. Slicing the paragraph text with them reproduces it exactly, so the
   *  pane can render line by line without the text drifting. */
  const lineSlices = useMemo(() => {
    if (!paragraphs) return null;
    return paragraphs.map((p) => {
      if (!p.lines || p.lines.length === 0) return null;
      let offset = 0;
      return p.lines.map((l) => {
        const slice = { start: offset, end: offset + l.len };
        offset = slice.end;
        return slice;
      });
    });
  }, [paragraphs]);

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

  // Rebuild the ordered line index after each render.
  useEffect(() => {
    lineNodesRef.current = [...lineRefs.current.entries()]
      .map(([key, el]) => {
        const [paragraph, line] = key.split(':').map(Number);
        return { paragraph, line, el };
      })
      .sort((a, b) => a.paragraph - b.paragraph || a.line - b.line);
  });

  const onVisibleRef = useRef(onVisibleParagraph);
  useEffect(() => { onVisibleRef.current = onVisibleParagraph; }, [onVisibleParagraph]);
  const onVisibleLineRef = useRef(onVisibleLine);
  useEffect(() => { onVisibleLineRef.current = onVisibleLine; }, [onVisibleLine]);
  const followingRef = useRef(followingPdf);
  useEffect(() => { followingRef.current = followingPdf; }, [followingPdf]);

  // Report this pane's own scroll position so the PDF can follow it. Only the
  // pane the user is actually scrolling reports (followingPdf is false), and
  // the parent's lock keeps the two directions from pushing each other.
  useEffect(() => {
    const el = scrollRef.current;
    if (!el) return;
    let ticking = false;
    const onScroll = () => {
      if (ticking) return;
      ticking = true;
      requestAnimationFrame(() => {
        ticking = false;
        if (followingRef.current) return;
        if (Date.now() < suppressReportUntilRef.current) return;
        const top = el.getBoundingClientRect().top;

        // Line level when the paragraphs carry anchors.
        const nodes = lineNodesRef.current;
        const lineCb = onVisibleLineRef.current;
        if (lineCb && nodes.length > 0) {
          let idx = firstLineBelow(nodes, top);
          // Anchors can be missing for a paragraph; walk on to the next line
          // that can be located on the PDF.
          while (idx >= 0 && idx < nodes.length) {
            const node = nodes[idx];
            const p = paragraphs?.[node.paragraph];
            const line = p?.lines?.[node.line];
            if (p?.bbox && line) {
              lineCb(node.paragraph, node.line, p, line);
              return;
            }
            idx += 1;
          }
          return;
        }

        const cb = onVisibleRef.current;
        if (!cb) return;
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

  // Reveal the active line (or paragraph). While the PDF is driving, align it to
  // the top instantly — smooth centering lags behind a fast scroll and can leave
  // the block off-screen, which is the whole point of the sync.
  useEffect(() => {
    const el = scrollRef.current;
    if (!el) return;
    const lineNode = activeLine
      ? lineRefs.current.get(`${activeLine.paragraph}:${activeLine.line}`)
      : undefined;
    const node = lineNode ?? (activeIndex != null ? paraRefs.current.get(activeIndex) : undefined);
    if (!node) return;
    suppressReportUntilRef.current = Date.now() + (followingPdf ? 200 : 800);
    if (followingPdf) {
      el.scrollTop += node.getBoundingClientRect().top - el.getBoundingClientRect().top - 8;
      return;
    }
    node.scrollIntoView({ block: 'center', behavior: 'smooth' });
  }, [activeIndex, activeLine, followingPdf]);

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
        const slices = lineSlices?.[i] ?? null;
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
                activeIndex === i && !activeLine
                  ? 'bg-primary/15 text-text-primary'
                  : 'text-text-primary/85 hover:bg-surface-hover'
              }`}
            >
              {slices && p.lines
                ? slices.map((slice, li) => {
                    const isActive = activeLine?.paragraph === i && activeLine.line === li;
                    return (
                      <div
                        key={li}
                        ref={(el) => {
                          const key = `${i}:${li}`;
                          if (el) lineRefs.current.set(key, el);
                          else lineRefs.current.delete(key);
                        }}
                        className={isActive ? 'bg-primary/20 rounded-sm' : undefined}
                      >
                        {p.text.slice(slice.start, slice.end)}
                      </div>
                    );
                  })
                : p.text}
            </div>
          </div>
        );
      })}
    </div>
  );
}
