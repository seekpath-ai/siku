import { useEffect, useRef, useState, useCallback } from 'react';

interface PdfThumbnailsProps {
  doc: any;
  currentPage: number;
  rotation?: number;
  onSelect: (page: number) => void;
}

const THUMBNAIL_WIDTH = 120;
const RENDER_AHEAD = 3;

export function PdfThumbnails({ doc, currentPage, rotation = 0, onSelect }: PdfThumbnailsProps) {
  const [pageCount, setPageCount] = useState(0);
  const [renderedPages, setRenderedPages] = useState<Set<number>>(new Set());
  const containerRef = useRef<HTMLDivElement>(null);
  const canvasMapRef = useRef<Map<number, HTMLCanvasElement>>(new Map());
  const visibleRef = useRef<Set<number>>(new Set());
  const mountedRef = useRef(true);

  useEffect(() => {
    setPageCount(doc.numPages || 0);
  }, [doc]);

  const renderThumbnail = useCallback(async (pageNum: number) => {
    if (canvasMapRef.current.has(pageNum)) return;
    try {
      const page = await doc.getPage(pageNum);
      const vp = page.getViewport({ scale: 1, rotation });
      const scale = THUMBNAIL_WIDTH / vp.width;
      const viewport = page.getViewport({ scale, rotation });
      const canvas = document.createElement('canvas');
      canvas.width = Math.floor(viewport.width);
      canvas.height = Math.floor(viewport.height);
      const ctx = canvas.getContext('2d');
      if (!ctx) return;
      await page.render({ canvasContext: ctx, viewport }).promise;
      canvasMapRef.current.set(pageNum, canvas);
      if (mountedRef.current) {
        setRenderedPages((prev) => {
          if (prev.has(pageNum)) return prev;
          const next = new Set(prev);
          next.add(pageNum);
          return next;
        });
      }
    } catch {
      // ignore
    }
  }, [doc, rotation]);

  useEffect(() => {
    mountedRef.current = true;
    return () => { mountedRef.current = false; };
  }, []);

  useEffect(() => {
    const container = containerRef.current;
    if (!container || pageCount === 0) return;

    const updateVisible = () => {
      const rect = container.getBoundingClientRect();
      const items = Array.from(container.children) as HTMLElement[];
      const visible = new Set<number>();
      items.forEach((el, idx) => {
        const pageNum = idx + 1;
        const r = el.getBoundingClientRect();
        if (r.bottom > rect.top - RENDER_AHEAD * 80 && r.top < rect.bottom + RENDER_AHEAD * 80) {
          visible.add(pageNum);
        }
      });
      visibleRef.current = visible;
      visible.forEach((p) => renderThumbnail(p));
    };

    const observer = new IntersectionObserver(
      (entries) => {
        entries.forEach((entry) => {
          const pageNum = parseInt((entry.target as HTMLElement).dataset.pageNum || '0', 10);
          if (entry.isIntersecting) visibleRef.current.add(pageNum);
          else visibleRef.current.delete(pageNum);
        });
        visibleRef.current.forEach((p) => renderThumbnail(p));
      },
      { root: container, rootMargin: `${RENDER_AHEAD * 80}px 0px` }
    );

    Array.from(container.children).forEach((el) => observer.observe(el));
    updateVisible();

    const onScroll = () => updateVisible();
    container.addEventListener('scroll', onScroll, { passive: true });
    return () => {
      observer.disconnect();
      container.removeEventListener('scroll', onScroll);
    };
  }, [pageCount, renderThumbnail]);

  // Re-render when rotation changes
  useEffect(() => {
    canvasMapRef.current.clear();
    setRenderedPages(new Set());
    visibleRef.current.forEach((p) => renderThumbnail(p));
  }, [rotation, renderThumbnail]);

  // Scroll current page into view
  useEffect(() => {
    const container = containerRef.current;
    if (!container || currentPage <= 0) return;
    const el = container.querySelector(`[data-page-num="${currentPage}"]`) as HTMLElement | null;
    if (el) el.scrollIntoView({ behavior: 'smooth', block: 'nearest' });
  }, [currentPage]);

  return (
    <div
      ref={containerRef}
      className="h-full overflow-auto py-2 flex flex-col items-center gap-2"
    >
      {Array.from({ length: pageCount }, (_, i) => i + 1).map((pageNum) => {
        const canvas = renderedPages.has(pageNum) ? canvasMapRef.current.get(pageNum) : undefined;
        return (
          <button
            key={pageNum}
            data-page-num={pageNum}
            onClick={() => onSelect(pageNum)}
            className={`relative w-[120px] rounded border bg-surface p-1 transition-colors ${
              pageNum === currentPage
                ? 'border-primary ring-1 ring-primary'
                : 'border-surface-hover hover:border-text-secondary/30'
            }`}
          >
            <div className="flex h-[140px] items-center justify-center overflow-hidden rounded">
              {canvas ? (
                <img
                  src={canvas.toDataURL('image/png')}
                  alt={`第 ${pageNum} 页`}
                  className="max-h-full max-w-full object-contain"
                />
              ) : (
                <span className="text-[10px] text-text-secondary/40">{pageNum}</span>
              )}
            </div>
            <span className="mt-1 block text-center text-[10px] text-text-secondary">
              {pageNum}
            </span>
          </button>
        );
      })}
    </div>
  );
}
