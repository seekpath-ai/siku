/* eslint-disable @typescript-eslint/no-explicit-any */
import { useEffect, useRef, useState, useCallback, forwardRef, useImperativeHandle } from 'react';
import { Loader2 } from 'lucide-react';
import { FindBar } from './FindBar';
import { TextHighlighter } from './TextHighlighter';
import { PasswordPrompt } from './PasswordPrompt';
import type { DetectedRegion, PageRegionResult, LlmRegionRequest } from './regions';
import { detectRegionsLlm } from '@/lib/tauri';
import { detectPageRegions } from './regionDetector';
import { createRegionOverlays, cleanupRegionOverlays } from './RegionOverlay';
import type { DrawingTool, Stroke } from './drawing';
import { buildPathD, generateStrokeId, strokeHitByEraser, ERASER_CURSOR, DEFAULT_PEN_COLOR, DEFAULT_PEN_WIDTH, DEFAULT_HIGHLIGHTER_WIDTH } from './drawing';
// Import the legacy build first so that globalThis.pdfjsLib is defined
// before the viewer module (which reads the global) is evaluated.
import 'pdfjs-dist/legacy/build/pdf.mjs';
import {
  EventBus,
  PDFFindController,
  AnnotationLayerBuilder,
  SimpleLinkService,
} from 'pdfjs-dist/legacy/web/pdf_viewer.mjs';

/** One highlight rectangle, stored as ratios relative to the page. */
export interface SnippetRect {
  /** Relative X center of the rect within the page (0 = left, 1 = right). */
  xRatio: number;
  /** Relative Y center of the rect within the page (0 = top, 1 = bottom). */
  yRatio: number;
  /** Relative width (0-1). */
  widthRatio: number;
  /** Relative height (0-1). */
  heightRatio: number;
}

/** W3C Web Annotation style text quote selector: enough context to
 * re-locate the selected text within the page's text content. */
export interface TextQuote {
  prefix: string;
  exact: string;
  suffix: string;
}

/** One contiguous segment of a multi-range (discontinuous) selection.
 * Rects and text are captured at mouseup and survive text-layer rebuilds. */
export interface SelectionSegment {
  /** PDF page number (1-based) this segment lives on */
  pageIndex: number;
  /** Normalized segment text */
  text: string;
  /** Per-line (text-layer span) rectangles of the segment, ratios relative
   * to that page. Used for snippet highlight; covers whole spans. */
  rects: SnippetRect[];
  /** Precise Range.getClientRects() rectangles, ratios relative to that
   * page. Used by the selection overlay so a partial-line selection does
   * not paint the whole line. */
  paintRects: SnippetRect[];
  quote: TextQuote;
}

export interface TextSelection {
  pageIndex: number;
  text: string;
  /** Relative Y position within the page (0 = top, 1 = bottom), zoom-independent */
  yRatio: number;
  /** Relative X position within the page (0 = left, 1 = right), used for column ordering */
  xRatio: number;
  /** Relative height of selection within the page (0-1), for highlight positioning */
  heightRatio: number;
  /** Relative width of selection within the page (0-1), for highlight positioning */
  widthRatio: number;
  /**
   * Per-line rectangles of the selection (one per text-layer span). A
   * selection spanning two columns yields two narrow rects instead of one
   * huge bounding box that covers the whole page.
   */
  rects: SnippetRect[];
  /**
   * All segments of a (possibly multi-range) selection, in page order.
   * Single-range selections yield exactly one segment; the top-level
   * fields above then mirror it for backward compatibility. Cross-page
   * drags are split into one segment per page.
   */
  segments: SelectionSegment[];
  /** Selection bounding rect in viewport coordinates (for toolbar positioning) */
  rect: { left: number; top: number; width: number; height: number };
}

export interface PdfViewerProps {
  src: string;
  initialPage?: number;
  initialZoom?: number;
  onPageChange?: (page: number) => void;
  onTotalPages?: (total: number) => void;
  onZoomChange?: (zoom: number) => void;
  onTextSelect?: (sel: TextSelection) => void;
  /** Called when all selection segments are cleared (plain click, dismiss). */
  onSelectionClear?: () => void;
  /** When set, shows blue highlight overlay(s) at the given page+positions.
   * Multiple targets highlight a multi-segment snippet across pages. */
  highlightTarget?: { pageIndex: number; rects: SnippetRect[] }[] | null;
  /** Increment to clear stored multi-range selection segments. */
  clearSelectionSignal?: number;
  /** When set, renders region detection overlays on the PDF pages */
  regionOverlays?: DetectedRegion[] | null;
  /** Active drawing tool. When null, the PDF page receives normal interactions. */
  drawingTool?: DrawingTool | null;
  /** Color used for pen / highlighter strokes. */
  drawingColor?: string;
  /** Existing strokes to render. */
  strokes?: Stroke[];
  /** Called when strokes are added or removed. */
  onStrokesChange?: (strokes: Stroke[]) => void;
  /** Password for encrypted PDFs. */
  password?: string;
  /** Called when the user submits a password so the parent can retry. */
  onPasswordSubmit?: (pw: string) => void;
  /** Called when the PDF document is loaded. */
  onDocumentLoaded?: (doc: any) => void;
  /** Recolor rendered pages via pdf.js pageColors. Null keeps original colors. */
  pageTheme?: { background: string; foreground: string } | null;
}

export interface PdfViewerHandle {
  zoomIn: () => void;
  zoomOut: () => void;
  jumpToPage: (page: number) => void;
  /** Locate a verbatim quote near a page hint and return overlay rects
   *  (probes the hinted page and its neighbors). Null when not found. */
  locateQuote: (pageNum: number, exact: string) => Promise<{ pageIndex: number; rects: SnippetRect[] } | null>;
  /** Run rule-based region detection on a page (fast, offline). Never calls LLM. */
  detectRegions: (pageNum: number) => Promise<PageRegionResult | null>;
  /** Refine detection for a page using LLM (requires API key). Returns null on failure. */
  refineWithLlm: (pageNum: number) => Promise<PageRegionResult | null>;
  /** Extract raw text content + intrinsic dimensions for any page. Used by export. */
  getPageTextContent: (pageNum: number) => Promise<{
    items: { str: string; transform: number[]; height?: number; fontName?: string; width?: number }[];
    width: number;
    height: number;
  } | null>;
  /** Access the loaded PDF document. */
  getDocument: () => any;
  setZoomMode: (mode: 'fit-width' | 'fit-page' | 'actual' | 'custom') => void;
  setZoom: (value: number) => void;
  rotateCw: () => void;
  rotateCcw: () => void;
}

const PAGE_GAP = 16;
const BUFFER_PAGES = 2;

interface PageSlot {
  pageNum: number;
  height: number;
}

/** Lightweight link service that satisfies PDF.js viewers at runtime. */
class SimpleReaderLinkService extends SimpleLinkService {
  pdfDocument: any = null;
  private jumpToPageFn: (page: number) => void;
  private getCurrentPageFn: () => number;
  private getPagesCountFn: () => number;
  private getRotationFn: () => number;
  private setRotationFn: (r: number) => void;

  constructor(
    eventBus: any,
    options: {
      jumpToPage: (page: number) => void;
      getCurrentPage: () => number;
      getPagesCount: () => number;
      getRotation: () => number;
      setRotation: (r: number) => void;
    }
  ) {
    super({ eventBus });
    this.jumpToPageFn = options.jumpToPage;
    this.getCurrentPageFn = options.getCurrentPage;
    this.getPagesCountFn = options.getPagesCount;
    this.getRotationFn = options.getRotation;
    this.setRotationFn = options.setRotation;
  }

  override setDocument(pdfDocument: any) {
    this.pdfDocument = pdfDocument;
  }

  // Base getter reads this.pdfViewer.isInPresentationMode when a document is
  // set, but we have no viewer instance — return false instead of crashing.
  override get isInPresentationMode(): boolean {
    return false;
  }

  override get pagesCount(): number {
    return this.pdfDocument?.numPages ?? this.getPagesCountFn() ?? 0;
  }

  override get page(): number {
    return this.getCurrentPageFn();
  }

  override set page(value: number) {
    this.jumpToPageFn(value);
  }

  override get rotation(): number {
    return this.getRotationFn();
  }

  override set rotation(value: number) {
    this.setRotationFn(value);
  }

  override goToPage(val: number | string) {
    const num = typeof val === 'string' ? parseInt(val, 10) : val;
    if (!Number.isFinite(num) || num < 1 || num > this.pagesCount) return;
    this.jumpToPageFn(num);
  }

  override async goToDestination(dest: string | any[]) {
    if (!this.pdfDocument) return;
    try {
      let explicit: any[];
      if (typeof dest === 'string') {
        explicit = await this.pdfDocument.getDestination(dest);
      } else {
        explicit = dest;
      }
      if (!Array.isArray(explicit) || explicit.length === 0) return;
      const pageRef = explicit[0];
      if (typeof pageRef === 'number' && Number.isInteger(pageRef)) {
        this.jumpToPageFn(pageRef + 1);
        return;
      }
      const pageIndex = await this.pdfDocument.getPageIndex(pageRef);
      if (typeof pageIndex === 'number') {
        this.jumpToPageFn(pageIndex + 1);
      }
    } catch {
      // ignore invalid destinations
    }
  }

  override addLinkAttributes(link: HTMLAnchorElement, url: string, newWindow = false) {
    link.href = url;
    if (newWindow || this.externalLinkTarget === 2) {
      link.target = '_blank';
    } else if (this.externalLinkTarget === 1) {
      link.target = '_self';
    }
    link.rel = this.externalLinkRel;
  }

  override getDestinationHash(_dest: string | any[]): string {
    return '#';
  }

  override getAnchorUrl(_anchor: string): string {
    return '#';
  }

  override setHash(_hash: string) {}
  override executeNamedAction(_action: string) {}
  override async executeSetOCGState(_action: any) {}
}

/** Simple URL regex for turning plain-text URLs in the text layer into
 *  clickable links. Matches http://, https:// and www. links. */
const URL_REGEX = /https?:\/\/[^\s<>"{}|\\^`\[\]]+|www\.[^\s<>"{}|\\^`\[\]]+/g;

/** Create clickable <a> overlays for plain URLs found in the text layer.
 *  PDFs often don't embed link annotations for bibliography URLs, so this
 *  makes those references clickable without changing the rendered text. */
function injectInferredLinks(
  textLayer: any,
  wrapper: HTMLDivElement,
  annotationDiv: HTMLDivElement
) {
  const textDivs: Node[] = textLayer.textDivs;
  const texts: string[] = textLayer.textContentItemsStr;
  if (!textDivs?.length || !texts?.length) return;

  const fullText = texts.join('\n');
  const matches = Array.from(fullText.matchAll(URL_REGEX));
  if (matches.length === 0) return;

  const wrapperRect = wrapper.getBoundingClientRect();

  for (const match of matches) {
    const url = match[0];
    const absUrl = url.startsWith('http') ? url : `https://${url}`;
    const startIdx = match.index ?? 0;
    const endIdx = startIdx + url.length;

    let pos = 0;
    let startDiv = -1;
    let startOff = 0;
    let endDiv = -1;
    let endOff = 0;
    for (let i = 0; i < texts.length; i++) {
      const str = texts[i];
      const len = str.length;
      if (startDiv === -1 && startIdx < pos + len) {
        startDiv = i;
        startOff = startIdx - pos;
      }
      if (endDiv === -1 && endIdx <= pos + len) {
        endDiv = i;
        endOff = endIdx - pos;
        break;
      }
      pos += len + 1; // +1 for the joining newline
    }
    if (startDiv === -1 || endDiv === -1) continue;

    try {
      const range = document.createRange();
      const startNode = textDivs[startDiv].firstChild;
      const endNode = textDivs[endDiv].firstChild;
      if (!startNode || !endNode) continue;
      const startLen = startNode.textContent?.length ?? 0;
      const endLen = endNode.textContent?.length ?? 0;
      range.setStart(startNode, Math.max(0, Math.min(startOff, startLen)));
      range.setEnd(endNode, Math.max(0, Math.min(endOff, endLen)));

      for (const r of range.getClientRects()) {
        if (r.width < 2 || r.height < 2) continue;
        const a = document.createElement('a');
        a.href = absUrl;
        a.target = '_blank';
        a.rel = 'noopener noreferrer nofollow';
        a.title = absUrl;
        a.style.cssText =
          `position:absolute;left:${r.left - wrapperRect.left}px;` +
          `top:${r.top - wrapperRect.top}px;` +
          `width:${r.width}px;height:${r.height}px;`;
        annotationDiv.appendChild(a);
      }
    } catch {
      // ignore invalid ranges
    }
  }
}

/** Parse "#rrggbb" into [r, g, b], or null for anything else. */
function hexToRgb(hex: string): [number, number, number] | null {
  const m = /^#?([0-9a-f]{6})$/i.exec(hex.trim());
  if (!m) return null;
  const v = parseInt(m[1], 16);
  return [(v >> 16) & 255, (v >> 8) & 255, v & 255];
}

/** Recolor a freshly rendered page canvas for a reader theme: map each
 *  pixel's luminance onto the background→foreground ramp (duotone). pdf.js
 *  fills the page background opaque white, so pixels are opaque and a pure
 *  luminance map is exact for text and antialiasing. Works entirely in
 *  device pixels, so it is correct at any devicePixelRatio. */
function applyPageThemeToCanvas(
  canvas: HTMLCanvasElement,
  theme: { background: string; foreground: string },
) {
  const bg = hexToRgb(theme.background);
  const fg = hexToRgb(theme.foreground);
  if (!bg || !fg) return;
  const ctx = canvas.getContext('2d');
  if (!ctx || canvas.width === 0 || canvas.height === 0) return;
  const img = ctx.getImageData(0, 0, canvas.width, canvas.height);
  const data = img.data;
  for (let i = 0; i < data.length; i += 4) {
    // Fast integer luma (0.2126/0.7152/0.0722 scaled by 256).
    const luma = (data[i] * 54 + data[i + 1] * 183 + data[i + 2] * 19) >> 8;
    const t = 1 - luma / 255; // 0 = paper (background), 1 = ink (foreground)
    data[i] = Math.round(bg[0] + (fg[0] - bg[0]) * t);
    data[i + 1] = Math.round(bg[1] + (fg[1] - bg[1]) * t);
    data[i + 2] = Math.round(bg[2] + (fg[2] - bg[2]) * t);
  }
  ctx.putImageData(img, 0, 0);
}

export const PdfViewer = forwardRef<PdfViewerHandle, PdfViewerProps>(
  function PdfViewer({
    src,
    initialPage,
    initialZoom,
    onPageChange,
    onTotalPages,
    onZoomChange,
    onTextSelect,
    onSelectionClear,
    highlightTarget,
    clearSelectionSignal,
    regionOverlays,
    drawingTool = null,
    drawingColor = DEFAULT_PEN_COLOR,
    strokes = [],
    onStrokesChange,
    password,
    onPasswordSubmit,
    onDocumentLoaded,
    pageTheme = null,
  }, ref) {
  const containerRef = useRef<HTMLDivElement>(null);
  const scrollRef = useRef<HTMLDivElement>(null);
  const pdfDocRef = useRef<any>(null);
  const pdfjsRef = useRef<any>(null);
  const pdfViewerRef = useRef<any>(null);
  const pageSlotsRef = useRef<PageSlot[]>([]);
  const wrapperMapRef = useRef<Map<number, HTMLDivElement>>(new Map());
  const textLayerMapRef = useRef<Map<number, any>>(new Map());
  const renderingRef = useRef<Set<number>>(new Set()); // prevent duplicate renders
  const zoomRef = useRef(initialZoom ?? 1);
  const zoomModeRef = useRef<'fit-width' | 'fit-page' | 'actual' | 'custom'>('fit-width');
  const pagesRotationRef = useRef(0);
  const containerWidthRef = useRef(800);
  const initialPageRef = useRef(initialPage);
  const didRestorePageRef = useRef(false);
  const pageThemeRef = useRef(pageTheme);

  // PDF.js native services
  const eventBusRef = useRef<any>(null);
  const linkServiceRef = useRef<any>(null);
  const findControllerRef = useRef<any>(null);
  const annotationBuilderMapRef = useRef<Map<number, AnnotationLayerBuilder>>(new Map());
  const highlighterMapRef = useRef<Map<number, any>>(new Map());

  // ── Drawing state ──
  const drawingActiveRef = useRef(false);
  const currentStrokeRef = useRef<Stroke | null>(null);
  const svgMapRef = useRef<Map<number, SVGSVGElement>>(new Map());
  const drawingToolRef = useRef(drawingTool);
  const drawingColorRef = useRef(drawingColor);
  const strokesRef = useRef(strokes);

  useEffect(() => { drawingToolRef.current = drawingTool; }, [drawingTool]);
  useEffect(() => { drawingColorRef.current = drawingColor; }, [drawingColor]);
  useEffect(() => { strokesRef.current = strokes; }, [strokes]);

  const [totalPages, setTotalPages] = useState(0);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [ready, setReady] = useState(false);
  const [layoutVersion, setLayoutVersion] = useState(0); // bumped on zoom/layout rebuild
  const currentPageRef = useRef(1);

  // ── Search state ──
  const searchQueryRef = useRef('');
  const searchTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const rawQueryRef = useRef('');

  const [searchOpen, setSearchOpen] = useState(false);
  const [searchQuery, setSearchQuery] = useState('');
  const [searchMatchIndex, setSearchMatchIndex] = useState(-1);
  const [searchTotalMatches, setSearchTotalMatches] = useState(0);
  const [caseSensitive, setCaseSensitive] = useState(false);

  // ── Password state ──
  const [passwordNeeded, setPasswordNeeded] = useState(false);
  const [passwordError, setPasswordError] = useState<string | null>(null);

  // ── Mirror PDF.js TextLayerBuilder: remove .selecting on any pointerup ──
  useEffect(() => {
    const onPointerUp = () => {
      document.querySelectorAll('.pdf-text-layer.selecting').forEach((el) => {
        el.classList.remove('selecting');
      });
    };
    document.addEventListener('pointerup', onPointerUp);
    window.addEventListener('blur', onPointerUp);
    return () => {
      document.removeEventListener('pointerup', onPointerUp);
      window.removeEventListener('blur', onPointerUp);
    };
  }, []);

  // ── Multi-range selection state ──
  // Browsers allow only ONE live Range, so discontinuous selection is kept
  // as a list of segments captured at mouseup. Segments store page-relative
  // ratio rects + a TextQuote, never a live Range, so they survive text
  // layer rebuilds (zoom, virtualization) and can be persisted as-is.
  const segmentsRef = useRef<SelectionSegment[]>([]);
  const [segmentsVersion, setSegmentsVersion] = useState(0);
  /** Ctrl/Cmd held at the start of the current drag gesture → append mode. */
  const appendModeRef = useRef(false);
  /** Repaint hook installed by the overlay effect below. */
  const repaintSelectionRef = useRef<() => void>(() => {});

  const clearSegments = useCallback(() => {
    if (segmentsRef.current.length === 0) return;
    segmentsRef.current = [];
    setSegmentsVersion((v) => v + 1);
    onSelectionClear?.();
  }, [onSelectionClear]);

  // Parent-driven clear (toolbar dismiss).
  const clearSignalRef = useRef(clearSelectionSignal);
  useEffect(() => {
    if (clearSignalRef.current === clearSelectionSignal) return;
    clearSignalRef.current = clearSelectionSignal;
    clearSegments();
  }, [clearSelectionSignal, clearSegments]);

  /** Aggregate stored segments into the TextSelection shape the toolbar and
   * snippet creation consume. The viewport rect of the LAST segment
   * positions the toolbar. */
  const buildAggregate = useCallback((): TextSelection | null => {
    const segments = segmentsRef.current;
    if (segments.length === 0) return null;
    const first = segments[0];
    const last = segments[segments.length - 1];

    // Viewport bounding box of the last segment (for toolbar placement).
    let vpRect = { left: 0, top: 0, width: 0, height: 0 };
    const lw = wrapperMapRef.current.get(last.pageIndex);
    if (lw && last.rects.length > 0) {
      const wr = lw.getBoundingClientRect();
      let minX = Infinity, minY = Infinity, maxX = -Infinity, maxY = -Infinity;
      for (const r of last.rects) {
        const x1 = (r.xRatio - r.widthRatio / 2) * wr.width;
        const y1 = (r.yRatio - r.heightRatio / 2) * wr.height;
        minX = Math.min(minX, x1); minY = Math.min(minY, y1);
        maxX = Math.max(maxX, x1 + r.widthRatio * wr.width);
        maxY = Math.max(maxY, y1 + r.heightRatio * wr.height);
      }
      vpRect = { left: wr.left + minX, top: wr.top + minY, width: maxX - minX, height: maxY - minY };
    }

    // Bounding box of the first segment, in page ratios (legacy fields).
    let bx = 0, by = 0, bw = 0, bh = 0;
    if (first.rects.length > 0) {
      let minX = Infinity, minY = Infinity, maxX = -Infinity, maxY = -Infinity;
      for (const r of first.rects) {
        minX = Math.min(minX, r.xRatio - r.widthRatio / 2);
        minY = Math.min(minY, r.yRatio - r.heightRatio / 2);
        maxX = Math.max(maxX, r.xRatio + r.widthRatio / 2);
        maxY = Math.max(maxY, r.yRatio + r.heightRatio / 2);
      }
      bx = (minX + maxX) / 2; by = (minY + maxY) / 2;
      bw = maxX - minX; bh = maxY - minY;
    }

    return {
      pageIndex: first.pageIndex,
      text: segments.map((s) => s.text).join('\n\n'),
      yRatio: by,
      xRatio: bx,
      heightRatio: bh,
      widthRatio: bw,
      rects: first.rects,
      segments,
      rect: vpRect,
    };
  }, []);

  /** Push the current aggregate to the parent (after capture or segment
   * deletion). Clears the parent's toolbar when nothing is left. */
  const emitSelection = useCallback(() => {
    const agg = buildAggregate();
    if (agg) onTextSelect?.(agg);
    else onSelectionClear?.();
  }, [buildAggregate, onTextSelect, onSelectionClear]);

  // Track the gesture modifier + handle plain-gesture clear and
  // Ctrl/Cmd+click segment removal. Capture phase so we can swallow the
  // pointerdown before the browser starts a new selection.
  useEffect(() => {
    const container = scrollRef.current;
    if (!container || !ready) return;
    const onPointerDown = (e: PointerEvent) => {
      appendModeRef.current = e.ctrlKey || e.metaKey;
      if (!appendModeRef.current) {
        clearSegments();
        return;
      }
      if (segmentsRef.current.length === 0) return;
      // Ctrl/Cmd+click on an existing segment removes it.
      for (const seg of segmentsRef.current) {
        const wrapper = wrapperMapRef.current.get(seg.pageIndex);
        if (!wrapper || !wrapper.querySelector('canvas')) continue;
        const wr = wrapper.getBoundingClientRect();
        const hit = seg.rects.some((r) => {
          const x = wr.left + (r.xRatio - r.widthRatio / 2) * wr.width;
          const y = wr.top + (r.yRatio - r.heightRatio / 2) * wr.height;
          return (
            e.clientX >= x && e.clientX <= x + r.widthRatio * wr.width &&
            e.clientY >= y && e.clientY <= y + r.heightRatio * wr.height
          );
        });
        if (hit) {
          segmentsRef.current = segmentsRef.current.filter((s) => s !== seg);
          setSegmentsVersion((v) => v + 1);
          emitSelection();
          e.preventDefault();
          e.stopPropagation();
          return;
        }
      }
    };
    container.addEventListener('pointerdown', onPointerDown, true);
    return () => container.removeEventListener('pointerdown', onPointerDown, true);
  }, [ready, clearSegments, emitSelection]);

  // Ctrl/Cmd+C copies all segments joined, even though the native selection
  // was released after capture.
  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      if (!(e.ctrlKey || e.metaKey) || e.key.toLowerCase() !== 'c') return;
      const tag = (e.target as HTMLElement)?.tagName;
      if (tag === 'INPUT' || tag === 'TEXTAREA') return;
      if (segmentsRef.current.length === 0) return;
      const text = segmentsRef.current.map((s) => s.text).join('\n\n');
      e.preventDefault();
      navigator.clipboard.writeText(text).catch(() => { /* ignore */ });
    };
    document.addEventListener('keydown', onKeyDown);
    return () => document.removeEventListener('keydown', onKeyDown);
  }, []);

  // ── Custom selection overlay ──
  // Blink/WebKit paint the native ::selection background at the font's
  // ascent+descent height, ignoring line-height, so for PDFs whose text
  // items include line leading the highlight fills the inter-line gap and
  // looks like a thick bar. The native background is hidden in index.css;
  // here we draw per-line overlay rects shrunk vertically to hug the glyphs.
  // Each page gets one <svg> whose <path> holds all rects and is filled in a
  // single paint operation: overlapping rects (font changes produce duplicate
  // boxes for the same span) can never double-darken the highlight.
  useEffect(() => {
    if (!ready) return;
    const container = scrollRef.current;
    if (!container) return;

    const HEIGHT_FACTOR = 0.8;
    const SVG_NS = 'http://www.w3.org/2000/svg';

    const clearOverlays = () => {
      container.querySelectorAll('.pdf-selection-overlay').forEach((el) => el.remove());
    };

    let raf = 0;
    const render = () => {
      raf = 0;
      clearOverlays();

      // Group shrunken rects per page wrapper as path segments.
      const segmentsByWrapper = new Map<HTMLDivElement, string[]>();
      const pushPath = (wrapper: HTMLDivElement, x: number, y: number, w: number, h: number) => {
        const seg =
          `M${x.toFixed(1)} ${y.toFixed(1)}` +
          `h${w.toFixed(1)}v${h.toFixed(1)}h${(-w).toFixed(1)}Z`;
        const list = segmentsByWrapper.get(wrapper);
        if (list) list.push(seg);
        else segmentsByWrapper.set(wrapper, [seg]);
      };

      // 1) Stored multi-range segments: page-ratio rects → wrapper coords.
      //    Painted on whichever pages are currently rendered. paintRects
      //    (precise client rects) are preferred over the span-level rects
      //    so partial-line selections do not paint whole lines.
      for (const seg of segmentsRef.current) {
        const wrapper = wrapperMapRef.current.get(seg.pageIndex);
        if (!wrapper || !wrapper.querySelector('canvas')) continue;
        const wr = wrapper.getBoundingClientRect();
        const paint = seg.paintRects.length > 0 ? seg.paintRects : seg.rects;
        for (const r of paint) {
          const w = r.widthRatio * wr.width;
          const h = r.heightRatio * wr.height * HEIGHT_FACTOR;
          const x = (r.xRatio - r.widthRatio / 2) * wr.width;
          const y = (r.yRatio * wr.height) - h / 2;
          pushPath(wrapper, x, y, w, h);
        }
      }

      // 2) The live selection while the user is dragging.
      const sel = window.getSelection();
      if (sel && !sel.isCollapsed && sel.rangeCount > 0) {
        const range = sel.getRangeAt(0);
        if (container.contains(range.commonAncestorContainer)) {
          const rects = Array.from(range.getClientRects())
            .filter((r) => r.width > 1 && r.height > 1);
          for (const r of rects) {
            const cx = r.left + r.width / 2;
            const cy = r.top + r.height / 2;
            for (const [, wrapper] of wrapperMapRef.current) {
              if (!wrapper.querySelector('canvas')) continue;
              const wr = wrapper.getBoundingClientRect();
              if (cx < wr.left || cx > wr.right || cy < wr.top || cy > wr.bottom) continue;
              const h = r.height * HEIGHT_FACTOR;
              pushPath(wrapper, r.left - wr.left, cy - wr.top - h / 2, r.width, h);
              break;
            }
          }
        }
      }

      for (const [wrapper, segments] of segmentsByWrapper) {
        const svg = document.createElementNS(SVG_NS, 'svg');
        svg.setAttribute('class', 'pdf-selection-overlay');
        svg.style.cssText = 'position:absolute;inset:0;width:100%;height:100%;pointer-events:none;z-index:5;';
        const path = document.createElementNS(SVG_NS, 'path');
        path.setAttribute('d', segments.join(''));
        svg.appendChild(path);
        wrapper.appendChild(svg);
      }
    };

    const onSelectionChange = () => {
      if (!raf) raf = requestAnimationFrame(render);
    };
    repaintSelectionRef.current = onSelectionChange;
    // Paint stored segments immediately (they do not fire selectionchange).
    onSelectionChange();
    document.addEventListener('selectionchange', onSelectionChange);
    return () => {
      document.removeEventListener('selectionchange', onSelectionChange);
      repaintSelectionRef.current = () => {};
      if (raf) cancelAnimationFrame(raf);
      clearOverlays();
    };
  }, [ready, segmentsVersion]);


  useEffect(() => {
    let cancelled = false;
    setLoading(true); setError(null); setReady(false); setTotalPages(0);
    setPasswordNeeded(false); setPasswordError(null);
    pdfDocRef.current = null;
    pdfjsRef.current = null;
    pdfViewerRef.current = null;
    currentPageRef.current = 1;
    pageSlotsRef.current = [];
    renderingRef.current.clear();
    for (const [, tl] of textLayerMapRef.current) tl.cancel?.();
    textLayerMapRef.current.clear();
    highlighterMapRef.current.clear();
    for (const [, b] of annotationBuilderMapRef.current) b.cancel?.();
    annotationBuilderMapRef.current.clear();
    wrapperMapRef.current.clear();
    eventBusRef.current = null;
    linkServiceRef.current = null;
    findControllerRef.current = null;

    (async () => {
      try {
        const [pdfjs, workerUrl, pdfViewer] = await Promise.all([
          import('pdfjs-dist/legacy/build/pdf.mjs'),
          import('pdfjs-dist/legacy/build/pdf.worker.min.mjs?url'),
          import('pdfjs-dist/legacy/web/pdf_viewer.mjs'),
        ]);
        if (cancelled) return;
        pdfjs.GlobalWorkerOptions.workerSrc = workerUrl.default;
        pdfjsRef.current = pdfjs;
        pdfViewerRef.current = pdfViewer;

        const loadArgs: any = { url: src };
        if (password) loadArgs.password = password;

        const doc = await pdfjs.getDocument(loadArgs).promise;
        if (cancelled) return;
        pdfDocRef.current = doc;
        setTotalPages(doc.numPages);
        onTotalPages?.(doc.numPages);
        onDocumentLoaded?.(doc);

        // Set up PDF.js native services
        const eventBus = new EventBus();
        eventBusRef.current = eventBus;

        const linkService = new SimpleReaderLinkService(eventBus, {
          jumpToPage: (page: number) => {
            const wrapper = wrapperMapRef.current.get(page);
            if (wrapper) wrapper.scrollIntoView({ behavior: 'smooth', block: 'start' });
          },
          getCurrentPage: () => currentPageRef.current,
          getPagesCount: () => doc.numPages,
          getRotation: () => pagesRotationRef.current,
          setRotation: (r: number) => { pagesRotationRef.current = r; },
        });
        linkService.setDocument(doc);
        linkServiceRef.current = linkService;

        const findController = new PDFFindController({
          linkService,
          eventBus,
        });
        findController.onIsPageVisible = (pageNumber: number) => {
          const [first, last] = getVisibleRange();
          return pageNumber >= first && pageNumber <= last;
        };
        findController.setDocument(doc);
        findControllerRef.current = findController;

        eventBus.on('updatefindmatchescount', (data: any) => {
          // Fires once text extraction finishes; carries the correct
          // current/total pair (updatefindcontrolstate can arrive earlier
          // with a zeroed current while extraction is still in progress).
          const mc = data.matchesCount;
          setSearchTotalMatches(mc?.total ?? 0);
          if (mc?.current >= 1) setSearchMatchIndex(mc.current - 1);
        });
        eventBus.on('updatefindcontrolstate', (data: any) => {
          const current = data.matchesCount?.current ?? 0;
          setSearchMatchIndex(current > 0 ? current - 1 : -1);
          if (data.rawQuery != null) rawQueryRef.current = data.rawQuery;
        });

        setLoading(false);
        setReady(true);
      } catch (e: any) {
        if (cancelled) return;
        if (e?.name === 'PasswordException') {
          setPasswordNeeded(true);
          setPasswordError(password ? '密码错误' : null);
          setLoading(false);
          return;
        }
        setError(e.message || 'Failed to load PDF');
        setLoading(false);
      }
    })();
    return () => { cancelled = true; };
  }, [src, password]);

  // ── Compute available width once on resize ──
  const computeAvailable = useCallback(() => {
    return Math.max(200, scrollRef.current?.clientWidth ?? 800);
  }, []);

  // ── Pre-calculate all page heights and create placeholder wrappers ──
  const buildLayout = useCallback(async () => {
    const doc = pdfDocRef.current;
    const container = scrollRef.current;
    if (!doc || !container) return;

    const available = computeAvailable();
    const zoom = zoomRef.current;
    const mode = zoomModeRef.current;
    const rotation = pagesRotationRef.current;
    const slots: PageSlot[] = [];

    // Clear old wrappers, text layers, drawing overlays, annotations and pending renders
    renderingRef.current.clear();
    for (const [, tl] of textLayerMapRef.current) tl.cancel?.();
    textLayerMapRef.current.clear();
    highlighterMapRef.current.clear();
    for (const [, b] of annotationBuilderMapRef.current) b.cancel?.();
    annotationBuilderMapRef.current.clear();
    svgMapRef.current.clear();
    for (const [, w] of wrapperMapRef.current) w.remove();
    wrapperMapRef.current.clear();
    container.innerHTML = '';

    for (let i = 1; i <= doc.numPages; i++) {
      const page = await doc.getPage(i);
      const vp1 = page.getViewport({ scale: 1, rotation });
      let baseScale: number;
      switch (mode) {
        case 'fit-page':
          baseScale = Math.min(available / vp1.width, available / vp1.height);
          break;
        case 'actual':
          baseScale = 1;
          break;
        case 'custom':
        case 'fit-width':
        default:
          baseScale = available / vp1.width;
          break;
      }
      const fitScale = Math.max(0.25, Math.min(4, baseScale * zoom));
      const height = Math.round(vp1.height * fitScale);
      slots.push({ pageNum: i, height });

      // Create wrapper (always stays in DOM for stable scroll height)
      const wrapper = document.createElement('div');
      wrapper.className = 'relative shadow-lg flex-shrink-0';
      wrapper.id = `pdf-page-${i}`;
      wrapper.dataset.pageNum = String(i);
      wrapper.style.width = `${Math.round(vp1.width * fitScale)}px`;
      wrapper.style.height = `${height}px`;
      wrapper.style.marginBottom = `${PAGE_GAP}px`;
      wrapper.style.overflow = 'hidden'; // prevent child content from expanding wrapper
      // Lightweight placeholder
      const ph = document.createElement('div');
      ph.className = 'flex items-center justify-center h-full text-text-secondary/30 text-xs select-none';
      ph.textContent = String(i);
      wrapper.appendChild(ph);
      container.appendChild(wrapper);
      wrapperMapRef.current.set(i, wrapper);
    }

    pageSlotsRef.current = slots;
    containerWidthRef.current = available;
    setLayoutVersion(v => v + 1); // trigger overlay re-creation after layout rebuild
  }, [computeAvailable]);

  useEffect(() => {
    if (ready) buildLayout();
  }, [ready, buildLayout]);

  // Notify parent of the initial zoom (which may have been restored from store)
  // and try to scroll to the restored page once the layout is built.
  useEffect(() => {
    if (!ready) return;
    onZoomChange?.(zoomRef.current);
  }, [ready, onZoomChange]);

  useEffect(() => {
    if (!ready || didRestorePageRef.current) return;
    const target = initialPageRef.current;
    if (!target || target <= 1) {
      didRestorePageRef.current = true;
      return;
    }
    // Wait for layout to be built (wrappers created).
    if (wrapperMapRef.current.size === 0) return;
    const wrapper = wrapperMapRef.current.get(target);
    if (wrapper) {
      wrapper.scrollIntoView({ behavior: 'auto', block: 'start' });
    }
    didRestorePageRef.current = true;
  }, [ready, layoutVersion]);

  // ── Render canvas + text layer + annotations into a wrapper ──
  async function renderPageInto(pageNum: number) {
    const doc = pdfDocRef.current;
    const wrapper = wrapperMapRef.current.get(pageNum);
    if (!doc || !wrapper) return;

    // Already rendered or currently rendering
    if (wrapper.querySelector('canvas') || renderingRef.current.has(pageNum)) return;

    renderingRef.current.add(pageNum);
    try {
      const available = containerWidthRef.current;
      const dpr = window.devicePixelRatio || 1;
      const zoom = zoomRef.current;
      const rotation = pagesRotationRef.current;
      const mode = zoomModeRef.current;

      const page = await doc.getPage(pageNum);
      const vp1 = page.getViewport({ scale: 1, rotation });
      let baseScale: number;
      switch (mode) {
        case 'fit-page':
          baseScale = Math.min(available / vp1.width, available / vp1.height);
          break;
        case 'actual':
          baseScale = 1;
          break;
        case 'custom':
        case 'fit-width':
        default:
          baseScale = available / vp1.width;
          break;
      }
      const fitScale = Math.max(0.25, Math.min(4, baseScale * zoom));
      const viewport = page.getViewport({ scale: fitScale, rotation });

      const canvas = document.createElement('canvas');
      canvas.style.display = 'block';
      canvas.style.width = `${viewport.width}px`;
      canvas.style.height = `${viewport.height}px`;
      canvas.width = Math.floor(viewport.width * dpr);
      canvas.height = Math.floor(viewport.height * dpr);
      canvas.dataset.page = String(pageNum);

      // willReadFrequently when a page theme is active: we read the pixels
      // back for recoloring right after the render.
      const ctx = canvas.getContext('2d', pageThemeRef.current ? { willReadFrequently: true } : undefined)!;
      ctx.scale(dpr, dpr);
      const renderParams: any = { canvasContext: ctx, viewport };
      await page.render(renderParams).promise;

      // Double-check still wanted (user may have scrolled past during render)
      if (!wrapperMapRef.current.has(pageNum)) { renderingRef.current.delete(pageNum); return; }

      // Recolor for the active page theme AFTER rendering. pdf.js
      // `pageColors` is broken on HiDPI screens (it recolors in CSS pixels
      // while the backing store is DPR-scaled, leaving most of the canvas
      // unpainted), so we duotone-map the pixels ourselves in device space.
      if (pageThemeRef.current) applyPageThemeToCanvas(canvas, pageThemeRef.current);

      // Replace placeholder with canvas
      const placeholder = wrapper.querySelector(':scope > div:not(.pdf-text-layer):not(.annotationLayer):not(.pdf-selection-overlay)');
      if (placeholder) placeholder.remove();
      wrapper.insertBefore(canvas, wrapper.firstChild);

      // Text layer (best-effort, async)
      await buildTextLayer(pageNum, page, viewport, wrapper).catch(() => {});

      // Annotation layer (placed above text layer so links are clickable).
      // AnnotationLayerBuilder renders into its own internally-created div,
      // which is only exposed via the onAppend callback — we must append that
      // div ourselves, otherwise link annotations never reach the DOM.
      let annotationDiv: HTMLDivElement | null = null;
      const builder = new AnnotationLayerBuilder({
        pdfPage: page,
        linkService: linkServiceRef.current,
        renderForms: true,
        imageResourcesPath: '',
        onAppend: (div: HTMLDivElement) => {
          div.style.cssText = 'position:absolute;inset:0;z-index:10;';
          wrapper.appendChild(div);
          annotationDiv = div;
        },
      });
      annotationBuilderMapRef.current.set(pageNum, builder);
      const annotationViewport = page.getViewport({ scale: fitScale, rotation }).clone({ dontFlip: true });
      await builder.render({ viewport: annotationViewport });
      annotationDiv = annotationDiv ?? builder.div ?? null;

      // Also make plain-text URLs clickable (bibliography links, etc.).
      const textLayer = textLayerMapRef.current.get(pageNum);
      if (textLayer && annotationDiv) injectInferredLinks(textLayer, wrapper, annotationDiv);

      // Keep drawing overlay on top of everything.
      const svg = svgMapRef.current.get(pageNum);
      if (svg) wrapper.appendChild(svg);
    } finally {
      renderingRef.current.delete(pageNum);
    }
  }

  // ── Remove canvas from a wrapper (keep placeholder for scroll height) ──
  function unrenderPage(pageNum: number) {
    const wrapper = wrapperMapRef.current.get(pageNum);
    if (!wrapper) return;

    // Cancel text layer for this page
    const textLayer = textLayerMapRef.current.get(pageNum);
    if (textLayer) {
      textLayer.cancel?.();
      textLayerMapRef.current.delete(pageNum);
    }
    highlighterMapRef.current.get(pageNum)?.disable();
    highlighterMapRef.current.delete(pageNum);

    const builder = annotationBuilderMapRef.current.get(pageNum);
    if (builder) {
      builder.cancel?.();
      annotationBuilderMapRef.current.delete(pageNum);
    }

    const canvas = wrapper.querySelector('canvas') as HTMLCanvasElement;
    if (!canvas) return;

    // Remove canvas and layers
    const textLayerDiv = wrapper.querySelector('.pdf-text-layer') as HTMLDivElement;
    if (textLayerDiv) textLayerDiv.remove();
    const annotationDiv = wrapper.querySelector('.annotationLayer') as HTMLDivElement;
    if (annotationDiv) annotationDiv.remove();
    wrapper.querySelectorAll('.pdf-selection-overlay').forEach((el) => el.remove());
    canvas.remove();

    // Restore placeholder
    const ph = document.createElement('div');
    ph.className = 'flex items-center justify-center h-full text-text-secondary/30 text-xs select-none';
    ph.textContent = String(pageNum);
    wrapper.appendChild(ph);
  }

  // ── Drawing overlay ──
  function ensureDrawingOverlay(pageNum: number) {
    const wrapper = wrapperMapRef.current.get(pageNum);
    if (!wrapper) return null;
    let svg = svgMapRef.current.get(pageNum);
    if (!svg || !wrapper.contains(svg)) {
      svg = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
      svg.classList.add('pdf-drawing-layer');
      svg.style.cssText = 'position:absolute;inset:0;width:100%;height:100%;z-index:20;touch-action:none;';
      svg.setAttribute('width', '100%');
      svg.setAttribute('height', '100%');
      svg.setAttribute('preserveAspectRatio', 'none');
      // Transparent full-size rect so the empty SVG still captures pointer events.
      // Its pointer-events are toggled together with the SVG based on the active tool.
      const hitRect = document.createElementNS('http://www.w3.org/2000/svg', 'rect');
      hitRect.classList.add('pdf-drawing-hitrect');
      hitRect.setAttribute('width', '100%');
      hitRect.setAttribute('height', '100%');
      hitRect.setAttribute('fill', 'transparent');
      svg.appendChild(hitRect);
      wrapper.appendChild(svg);
      svgMapRef.current.set(pageNum, svg);
      attachDrawingListeners(svg, wrapper, pageNum);
    }
    return svg;
  }

  function eventToRatio(e: PointerEvent, wrapper: HTMLDivElement) {
    const rect = wrapper.getBoundingClientRect();
    return {
      xRatio: Math.max(0, Math.min(1, (e.clientX - rect.left) / rect.width)),
      yRatio: Math.max(0, Math.min(1, (e.clientY - rect.top) / rect.height)),
    };
  }

  function attachDrawingListeners(svg: SVGSVGElement, wrapper: HTMLDivElement, pageNum: number) {
    const onPointerDown = (e: PointerEvent) => {
      const tool = drawingToolRef.current;
      if (!tool) return;
      e.preventDefault();
      e.stopPropagation();
      svg.setPointerCapture(e.pointerId);
      drawingActiveRef.current = true;
      const p = eventToRatio(e, wrapper);
      if (tool === 'eraser') {
        currentStrokeRef.current = { id: generateStrokeId(), pageIndex: pageNum, tool: 'pen', color: '', width: 0, points: [p] };
        return;
      }
      currentStrokeRef.current = {
        id: generateStrokeId(),
        pageIndex: pageNum,
        tool,
        color: drawingColorRef.current,
        width: tool === 'highlighter' ? DEFAULT_HIGHLIGHTER_WIDTH : DEFAULT_PEN_WIDTH,
        points: [p],
      };
      renderCurrentStroke();
    };

    const onPointerMove = (e: PointerEvent) => {
      if (!drawingActiveRef.current || !currentStrokeRef.current) return;
      e.preventDefault();
      const p = eventToRatio(e, wrapper);
      // Avoid duplicate points
      const last = currentStrokeRef.current.points[currentStrokeRef.current.points.length - 1];
      if (last && Math.hypot(last.xRatio - p.xRatio, last.yRatio - p.yRatio) < 0.002) return;
      currentStrokeRef.current.points.push(p);
      // The eraser does not draw; only pen / highlighter show a live preview.
      if (drawingToolRef.current !== 'eraser') renderCurrentStroke();
    };

    const onPointerUp = (e: PointerEvent) => {
      if (!drawingActiveRef.current || !currentStrokeRef.current) return;
      e.preventDefault();
      drawingActiveRef.current = false;
      svg.releasePointerCapture(e.pointerId);
      const stroke = currentStrokeRef.current;
      // Remove the live preview by page BEFORE nulling the ref — the preview
      // element would otherwise linger and keep the last-drawn stroke visible
      // even after it is erased.
      removeCurrentStrokePreview(pageNum);
      currentStrokeRef.current = null;
      const current = strokesRef.current;
      // The eraser path is tagged with tool 'pen' to reuse the stroke shape,
      // so check the active tool first — an eraser gesture must never be
      // appended as a drawn stroke.
      if (drawingToolRef.current === 'eraser') {
        const toRemove = new Set(current.filter(s => s.pageIndex === pageNum && strokeHitByEraser(s, stroke.points)).map(s => s.id));
        if (toRemove.size > 0) {
          onStrokesChange?.(current.filter(s => !toRemove.has(s.id)));
        }
      } else if (stroke.tool === 'pen' || stroke.tool === 'highlighter') {
        if (stroke.points.length < 2) return;
        onStrokesChange?.([...current, stroke]);
      }
    };

    svg.addEventListener('pointerdown', onPointerDown);
    svg.addEventListener('pointermove', onPointerMove);
    svg.addEventListener('pointerup', onPointerUp);
    svg.addEventListener('pointercancel', onPointerUp);
    svg.addEventListener('pointerleave', (e) => {
      if (drawingActiveRef.current && currentStrokeRef.current) onPointerUp(e as PointerEvent);
    });
  }

  function removeCurrentStrokePreview(pageIndex?: number) {
    const page = pageIndex ?? currentStrokeRef.current?.pageIndex;
    if (!page) return;
    const svg = svgMapRef.current.get(page);
    if (!svg) return;
    const preview = svg.querySelector('.drawing-current-preview');
    if (preview) preview.remove();
  }

  function renderCurrentStroke() {
    const stroke = currentStrokeRef.current;
    if (!stroke) return;
    const svg = ensureDrawingOverlay(stroke.pageIndex);
    if (!svg) return;
    let path = svg.querySelector('.drawing-current-preview') as SVGPathElement | null;
    if (!path) {
      path = document.createElementNS('http://www.w3.org/2000/svg', 'path');
      path.classList.add('drawing-current-preview');
      path.setAttribute('fill', 'none');
      path.setAttribute('stroke-linecap', 'round');
      path.setAttribute('stroke-linejoin', 'round');
      svg.appendChild(path);
    }
    const w = svg.clientWidth || svg.getBoundingClientRect().width || 1;
    const h = svg.clientHeight || svg.getBoundingClientRect().height || 1;
    path.setAttribute('d', buildPathD(stroke.points, w, h));
    if (stroke.tool === 'highlighter') {
      path.setAttribute('stroke', stroke.color);
      path.setAttribute('stroke-width', String(DEFAULT_HIGHLIGHTER_WIDTH));
      path.setAttribute('opacity', '0.4');
    } else {
      path.setAttribute('stroke', stroke.color);
      path.setAttribute('stroke-width', String(DEFAULT_PEN_WIDTH));
      path.setAttribute('opacity', '1');
    }
  }

  function renderStrokes() {
    for (const [pageNum, svg] of svgMapRef.current) {
      // Remove old stroke paths (keep current preview)
      svg.querySelectorAll('.drawing-stroke').forEach(el => el.remove());
      const wrapper = wrapperMapRef.current.get(pageNum);
      if (!wrapper) continue;
      const w = wrapper.clientWidth;
      const h = wrapper.clientHeight;
      for (const stroke of strokes) {
        if (stroke.pageIndex !== pageNum || stroke.points.length < 2) continue;
        const path = document.createElementNS('http://www.w3.org/2000/svg', 'path');
        path.classList.add('drawing-stroke');
        path.setAttribute('d', buildPathD(stroke.points, w, h));
        path.setAttribute('fill', 'none');
        path.setAttribute('stroke-linecap', 'round');
        path.setAttribute('stroke-linejoin', 'round');
        path.setAttribute('stroke', stroke.color);
        path.setAttribute('stroke-width', String(stroke.width));
        if (stroke.tool === 'highlighter') {
          path.setAttribute('opacity', '0.4');
        }
        svg.appendChild(path);
      }
    }
  }

  // Create drawing overlays for all wrappers after layout builds.
  useEffect(() => {
    if (!ready) return;
    for (let i = 1; i <= (pdfDocRef.current?.numPages || 0); i++) {
      ensureDrawingOverlay(i);
    }
    renderStrokes();
    // Update pointer-events and cursor based on active tool.
    // Both the SVG and its hit rect must be disabled when no tool is active,
    // otherwise the rect (a descendant) still intercepts pointer events and
    // blocks PDF text selection.
    const cursor = drawingTool === 'eraser' ? ERASER_CURSOR : drawingTool ? 'crosshair' : 'default';
    for (const svg of svgMapRef.current.values()) {
      svg.style.pointerEvents = drawingTool ? 'auto' : 'none';
      svg.style.cursor = cursor;
      const hitRect = svg.querySelector('.pdf-drawing-hitrect') as SVGRectElement | null;
      if (hitRect) hitRect.style.pointerEvents = drawingTool ? 'all' : 'none';
    }
  }, [ready, layoutVersion, drawingTool]);

  // Re-render strokes when they change.
  useEffect(() => {
    renderStrokes();
  }, [strokes]);

  // ── Build text layer using pdf.js TextLayer (handles fonts, widths, RTL, marked content) ──
  async function buildTextLayer(pageNum: number, page: any, viewport: any, wrapper: HTMLDivElement) {
    // Cancel any existing text layer for this page
    const existing = textLayerMapRef.current.get(pageNum);
    if (existing) {
      existing.cancel?.();
      textLayerMapRef.current.delete(pageNum);
    }
    highlighterMapRef.current.get(pageNum)?.disable();
    highlighterMapRef.current.delete(pageNum);

    // Remove stale text layer div
    const oldDiv = wrapper.querySelector('.pdf-text-layer') as HTMLDivElement;
    if (oldDiv) oldDiv.remove();

    // Create container
    const textLayerDiv = document.createElement('div');
    textLayerDiv.className = 'pdf-text-layer';
    textLayerDiv.style.setProperty('--total-scale-factor', String(viewport.scale));
    textLayerDiv.style.setProperty('--scale-round-x', '1px');
    textLayerDiv.style.setProperty('--scale-round-y', '1px');
    wrapper.appendChild(textLayerDiv);
    // Keep the drawing SVG on top of the text layer.
    const svg = svgMapRef.current.get(pageNum);
    if (svg) wrapper.appendChild(svg);

    try {
      const textContent = await page.getTextContent();
      if (!textContent?.items?.length) return;

      const textLayer = new pdfjsRef.current.TextLayer({
        textContentSource: textContent,
        container: textLayerDiv,
        viewport,
      });
      await textLayer.render();
      textLayerMapRef.current.set(pageNum, textLayer);

      // Set up find-match highlighting for this page. pdfjs-dist does not
      // export its TextHighlighter, so we use the local port.
      if (findControllerRef.current && eventBusRef.current) {
        const existingHighlighter = highlighterMapRef.current.get(pageNum);
        if (existingHighlighter) {
          existingHighlighter.disable();
          highlighterMapRef.current.delete(pageNum);
        }
        const highlighter = new TextHighlighter(
          findControllerRef.current,
          eventBusRef.current,
          pageNum - 1,
        );
        highlighter.setTextMapping(textLayer.textDivs, textLayer.textContentItemsStr);
        highlighter.enable();
        highlighterMapRef.current.set(pageNum, highlighter);
      }

      // Append the invisible end-of-content element used by the official viewer
      // to let the user drag a selection to the bottom of the page.
      const endOfContent = document.createElement('div');
      endOfContent.className = 'endOfContent';
      textLayerDiv.appendChild(endOfContent);

      // Add/remove the .selecting class while the user is dragging a selection,
      // matching the official PDF.js TextLayerBuilder behavior.
      const onPointerDown = () => textLayerDiv.classList.add('selecting');
      const onPointerUp = () => textLayerDiv.classList.remove('selecting');
      textLayerDiv.addEventListener('pointerdown', onPointerDown);
      textLayerDiv.addEventListener('pointerup', onPointerUp);
      textLayerDiv.addEventListener('pointercancel', onPointerUp);

      // Normalize text copied with Ctrl+C so it behaves like the official viewer
      // (collapse line-wraps, remove hyphenation breaks).
      const onCopy = (event: ClipboardEvent) => {
        const selection = window.getSelection();
        if (!selection || selection.isCollapsed) return;
        const raw = selection.toString();
        if (!raw) return;
        event.preventDefault();
        event.clipboardData?.setData('text/plain', normalizePdfSelection(raw));
      };
      textLayerDiv.addEventListener('copy', onCopy);
    } catch {
      textLayerMapRef.current.delete(pageNum);
    }
    // A (re)built text layer wiped the page's selection overlay; repaint any
    // stored multi-range segments that live on this page.
    repaintSelectionRef.current();
  }

  // ── Determine visible page range using DOM positions (always accurate) ──
  const getVisibleRange = useCallback((): [number, number] => {
    const container = scrollRef.current;
    if (!container || wrapperMapRef.current.size === 0) return [1, 1];

    const containerRect = container.getBoundingClientRect();
    const viewTop = containerRect.top - containerRect.height * BUFFER_PAGES;
    const viewBottom = containerRect.bottom + containerRect.height * BUFFER_PAGES;

    let firstVisible = 1;
    let lastVisible = 1;
    let foundFirst = false;

    for (const wrapper of wrapperMapRef.current.values()) {
      const pn = parseInt(wrapper.dataset.pageNum || '0', 10);
      if (!pn) continue;
      const r = wrapper.getBoundingClientRect();
      if (r.bottom > viewTop && r.top < viewBottom) {
        if (!foundFirst) { firstVisible = pn; foundFirst = true; }
        lastVisible = pn;
      } else if (foundFirst && r.top >= viewBottom) {
        // Past the visible zone, stop scanning
        break;
      }
    }
    return [firstVisible, lastVisible];
  }, []);

  // ── Scroll anchor: which page + vertical ratio is at the top of the viewport ──
  const getScrollAnchor = useCallback((): { pageIndex: number; yRatio: number } | null => {
    const container = scrollRef.current;
    if (!container || wrapperMapRef.current.size === 0) return null;
    const viewTop = container.getBoundingClientRect().top + 4;
    for (const wrapper of wrapperMapRef.current.values()) {
      const pn = parseInt(wrapper.dataset.pageNum || '0', 10);
      if (!pn) continue;
      const r = wrapper.getBoundingClientRect();
      if (r.bottom > viewTop) {
        const yRatio = Math.max(0, Math.min(1, (viewTop - r.top) / Math.max(1, r.height)));
        return { pageIndex: pn, yRatio };
      }
    }
    return null;
  }, []);

  const restoreScrollAnchor = useCallback((anchor: { pageIndex: number; yRatio: number }) => {
    const container = scrollRef.current;
    const wrapper = wrapperMapRef.current.get(anchor.pageIndex);
    if (!container || !wrapper) return;
    container.scrollTop = wrapper.offsetTop + wrapper.offsetHeight * anchor.yRatio - 4;
  }, []);

  // ── Manage visible pages (with scroll position preservation) ──
  const updateVisiblePages = useCallback(() => {
    if (!pdfDocRef.current || wrapperMapRef.current.size === 0) return;

    const container = scrollRef.current;
    if (!container) return;

    // Preserve scroll position during DOM mutations
    const savedScrollTop = container.scrollTop;
    const savedScrollHeight = container.scrollHeight;

    const [first, last] = getVisibleRange();
    const shouldRender = new Set<number>();
    for (let i = first; i <= last; i++) shouldRender.add(i);

    // Unrender pages outside visible range
    for (const wrapper of wrapperMapRef.current.values()) {
      const pn = parseInt(wrapper.dataset.pageNum || '0', 10);
      if (!shouldRender.has(pn) && wrapper.querySelector('canvas')) {
        unrenderPage(pn);
      }
    }

    // Render newly visible pages
    for (const pn of shouldRender) {
      renderPageInto(pn);
    }

    // Restore scroll position if layout shifted
    if (container.scrollHeight !== savedScrollHeight) {
      container.scrollTop = savedScrollTop;
    }
  }, [getVisibleRange]);

  // ── Scroll: track current page + manage visible ──
  useEffect(() => {
    const container = scrollRef.current;
    if (!container || totalPages === 0) return;

    // Initial render
    const tid = setTimeout(updateVisiblePages, 200);

    let ticking = false;
    const onScroll = () => {
      if (ticking) return;
      ticking = true;
      requestAnimationFrame(() => {
        updateVisiblePages();

        // Current page detection
        const cr = container.getBoundingClientRect();
        const vt = cr.top;
        let found = 1;
        for (let i = 1; i <= totalPages; i++) {
          const w = wrapperMapRef.current.get(i);
          if (!w) continue;
          const r = w.getBoundingClientRect();
          if (r.bottom > vt + 10) { found = i; break; }
        }
        if (found !== currentPageRef.current) {
          currentPageRef.current = found;
          onPageChange?.(found);
        }
        ticking = false;
      });
    };
    container.addEventListener('scroll', onScroll, { passive: true });
    return () => { container.removeEventListener('scroll', onScroll); clearTimeout(tid); };
  }, [totalPages, onPageChange, updateVisiblePages]);

  // ── Text selection capture ──
  useEffect(() => {
    const container = scrollRef.current;
    if (!container || !onTextSelect) return;

    const onMouseUp = () => {
      // Delay to let browser finalize the selection
      setTimeout(() => {
        const sel = window.getSelection();
        if (!sel || sel.isCollapsed || !sel.toString().trim()) return;
        const range = sel.getRangeAt(0);
        if (!container.contains(range.commonAncestorContainer)) return;

        // Split the live range per rendered page text layer, so a drag that
        // spans pages yields one segment per page. Each segment's per-line
        // rects drive the highlight overlay — a two-column selection yields
        // one rect per column instead of one huge bounding box.
        const newSegments: SelectionSegment[] = [];
        for (const [pageNum, wrapper] of wrapperMapRef.current) {
          if (!wrapper.querySelector('canvas')) continue;
          const textLayer = wrapper.querySelector('.pdf-text-layer');
          if (!textLayer || !range.intersectsNode(textLayer)) continue;

          // Clamp the range to this page's text layer.
          const sub = range.cloneRange();
          const layerRange = document.createRange();
          layerRange.selectNodeContents(textLayer);
          if (sub.compareBoundaryPoints(Range.START_TO_START, layerRange) < 0) {
            sub.setStart(layerRange.startContainer, layerRange.startOffset);
          }
          if (sub.compareBoundaryPoints(Range.END_TO_END, layerRange) > 0) {
            sub.setEnd(layerRange.endContainer, layerRange.endOffset);
          }

          const rawText = sub.toString().trim();
          if (!rawText) continue;
          const text = normalizePdfSelection(rawText);

          const wrapperRect = wrapper.getBoundingClientRect();
          const subRect = sub.getBoundingClientRect();
          const rects: SnippetRect[] = [];
          if (wrapperRect.width > 0 && wrapperRect.height > 0) {
            const spans = Array.from(textLayer.querySelectorAll('span'));
            for (const span of spans) {
              // Only include spans the selection actually touches. The range
              // covers the wrapped text content; span-level intersection avoids
              // counting untouched lines between the two selection extremes.
              if (!sub.intersectsNode(span)) continue;
              const r = span.getBoundingClientRect();
              if (r.width <= 0 || r.height <= 0) continue;
              rects.push({
                xRatio: Math.max(0, Math.min(1, (r.left + r.width / 2 - wrapperRect.left) / wrapperRect.width)),
                yRatio: Math.max(0, Math.min(1, (r.top + r.height / 2 - wrapperRect.top) / wrapperRect.height)),
                widthRatio: Math.max(0, Math.min(1, r.width / wrapperRect.width)),
                heightRatio: Math.max(0, Math.min(1, r.height / wrapperRect.height)),
              });
            }
            // Keep visual order: top → bottom, then left → right.
            rects.sort((a, b) => a.yRatio - b.yRatio || a.xRatio - b.xRatio);
          }
          // Fallback: if the text layer spans could not be resolved, use the
          // single bounding box so the snippet still highlights something.
          if (rects.length === 0) {
            rects.push({
              xRatio: Math.max(0, Math.min(1, (subRect.left + subRect.width / 2 - wrapperRect.left) / Math.max(1, wrapperRect.width))),
              yRatio: Math.max(0, Math.min(1, (subRect.top + subRect.height / 2 - wrapperRect.top) / Math.max(1, wrapperRect.height))),
              widthRatio: Math.max(0, Math.min(1, subRect.width / Math.max(1, wrapperRect.width))),
              heightRatio: Math.max(0, Math.min(1, subRect.height / Math.max(1, wrapperRect.height))),
            });
          }

          // Precise client rects for the selection overlay: unlike the
          // span-level rects above these hug the actual selected portion,
          // so selecting part of a line does not paint the whole line.
          const paintRects: SnippetRect[] = [];
          if (wrapperRect.width > 0 && wrapperRect.height > 0) {
            for (const r of Array.from(sub.getClientRects())) {
              if (r.width <= 1 || r.height <= 1) continue;
              paintRects.push({
                xRatio: Math.max(0, Math.min(1, (r.left + r.width / 2 - wrapperRect.left) / wrapperRect.width)),
                yRatio: Math.max(0, Math.min(1, (r.top + r.height / 2 - wrapperRect.top) / wrapperRect.height)),
                widthRatio: Math.max(0, Math.min(1, r.width / wrapperRect.width)),
                heightRatio: Math.max(0, Math.min(1, r.height / wrapperRect.height)),
              });
            }
          }

          // TextQuote selector from the page's text content items: the exact
          // substring plus ±32 chars of context, enough to re-locate the
          // segment in the page text later.
          const tl = textLayerMapRef.current.get(pageNum);
          const pageText: string = tl?.textContentItemsStr?.join('\n') ?? '';
          const idx = pageText.indexOf(rawText);
          const quote: TextQuote = idx >= 0
            ? {
                prefix: pageText.slice(Math.max(0, idx - 32), idx),
                exact: rawText,
                suffix: pageText.slice(idx + rawText.length, idx + rawText.length + 32),
              }
            : { prefix: '', exact: rawText, suffix: '' };

          newSegments.push({
            pageIndex: pageNum, text, rects,
            paintRects: paintRects.length > 0 ? paintRects : rects,
            quote,
          });
        }
        if (newSegments.length === 0) return;

        if (appendModeRef.current) {
          // Append: drop stored segments identical to a new one, then merge
          // in page order.
          const merged = segmentsRef.current.filter(
            (old) => !newSegments.some((n) => n.pageIndex === old.pageIndex && n.text === old.text),
          );
          segmentsRef.current = [...merged, ...newSegments]
            .sort((a, b) => a.pageIndex - b.pageIndex);
        } else {
          segmentsRef.current = newSegments
            .sort((a, b) => a.pageIndex - b.pageIndex);
        }
        appendModeRef.current = false;
        // The native selection has been snapshotted into ratio rects; release
        // it so the next drag starts clean. The overlay keeps painting from
        // the stored segments.
        sel.removeAllRanges();
        setSegmentsVersion((v) => v + 1);

        const agg = buildAggregate();
        if (agg) onTextSelect(agg);
      }, 10);
    };

    container.addEventListener('mouseup', onMouseUp);
    return () => container.removeEventListener('mouseup', onMouseUp);
  }, [onTextSelect, buildAggregate]);

  // ── Ctrl+F / Cmd+F to open search ──
  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      if ((e.ctrlKey || e.metaKey) && e.key === 'f') {
        // Don't capture if focus is in another input/textarea
        const tag = (e.target as HTMLElement)?.tagName;
        if (tag === 'INPUT' || tag === 'TEXTAREA') return;
        e.preventDefault();
        openSearch();
      }
    };
    document.addEventListener('keydown', onKeyDown);
    return () => document.removeEventListener('keydown', onKeyDown);
  }, []);

  // ── Search: open/close/navigate ──
  function dispatchFind(opts: any) {
    if (!eventBusRef.current || !findControllerRef.current) return;
    eventBusRef.current.dispatch('find', {
      source: 'siku',
      query: searchQueryRef.current,
      caseSensitive,
      entireWord: false,
      highlightAll: true,
      findPrevious: false,
      ...opts,
    });
  }

  function openSearch() {
    setSearchOpen(true);
    if (searchQueryRef.current.trim()) {
      dispatchFind({});
    }
  }

  function closeSearch() {
    if (eventBusRef.current) {
      eventBusRef.current.dispatch('findbarclose');
    }
    setSearchOpen(false);
    setSearchQuery('');
    searchQueryRef.current = '';
    setSearchMatchIndex(-1);
    setSearchTotalMatches(0);
  }

  function navigateSearch(direction: 1 | -1) {
    dispatchFind({ type: 'again', findPrevious: direction < 0 });
  }

  // ── Debounced search query handler ──
  const handleSearchQueryChange = useCallback((query: string) => {
    setSearchQuery(query);
    searchQueryRef.current = query;
    if (searchTimerRef.current) clearTimeout(searchTimerRef.current);
    searchTimerRef.current = setTimeout(() => {
      dispatchFind({ query });
    }, 200);
  }, [caseSensitive]);

  const handleCaseSensitiveChange = useCallback((value: boolean) => {
    setCaseSensitive(value);
    if (searchQueryRef.current.trim()) {
      dispatchFind({ query: searchQueryRef.current, caseSensitive: value });
    }
  }, []);

  // ── Resize: rebuild layout (ResizeObserver catches both window & panel resizes) ──
  useEffect(() => {
    const el = containerRef.current;
    if (!ready || !el) return;

    let timer: any = null;
    const observer = new ResizeObserver(() => {
      clearTimeout(timer);
      timer = setTimeout(async () => {
        if (!pdfDocRef.current) return;
        const anchor = getScrollAnchor();
        await buildLayout();
        if (anchor) restoreScrollAnchor(anchor);
        updateVisiblePages();
      }, 150);
    });
    observer.observe(el);
    return () => { observer.disconnect(); clearTimeout(timer); };
  }, [ready, buildLayout, updateVisiblePages, getScrollAnchor, restoreScrollAnchor]);

  // ── Page theme: re-render visible pages when the theme changes ──
  useEffect(() => {
    const prev = pageThemeRef.current;
    // The parent may hand us a fresh object on every render; only re-render
    // pages when the colors actually changed, otherwise unrelated re-renders
    // (e.g. text selection) would wipe the text layer and kill the selection.
    const changed =
      prev?.background !== pageTheme?.background ||
      prev?.foreground !== pageTheme?.foreground;
    pageThemeRef.current = pageTheme;
    if (!ready || !changed) return;
    for (const [pn, wrapper] of wrapperMapRef.current) {
      if (wrapper.querySelector('canvas')) unrenderPage(pn);
    }
    updateVisiblePages();
  }, [pageTheme, ready, updateVisiblePages]);

  // ── Zoom / rotation helpers ──
  const doZoom = useCallback(async (delta: number) => {
    const doc = pdfDocRef.current;
    if (!doc) return;
    const available = containerWidthRef.current;
    const page = await doc.getPage(1);
    const vp1 = page.getViewport({ scale: 1, rotation: pagesRotationRef.current });
    let baseScale = available / vp1.width;
    if (zoomModeRef.current === 'fit-page') {
      baseScale = Math.min(available / vp1.width, available / vp1.height);
    } else if (zoomModeRef.current === 'actual') {
      baseScale = 1;
    }
    const currentScale = baseScale * zoomRef.current;
    zoomModeRef.current = 'custom';
    zoomRef.current = Math.max(0.25, Math.min(4, (currentScale + delta) / baseScale));
    onZoomChange?.(zoomRef.current);
    const anchor = getScrollAnchor();
    await buildLayout();
    if (anchor) restoreScrollAnchor(anchor);
    updateVisiblePages();
  }, [buildLayout, updateVisiblePages, onZoomChange, getScrollAnchor, restoreScrollAnchor]);

  const setZoomMode = useCallback(async (mode: 'fit-width' | 'fit-page' | 'actual' | 'custom') => {
    zoomModeRef.current = mode;
    zoomRef.current = 1;
    onZoomChange?.(zoomRef.current);
    const anchor = getScrollAnchor();
    await buildLayout();
    if (anchor) restoreScrollAnchor(anchor);
    updateVisiblePages();
  }, [buildLayout, updateVisiblePages, onZoomChange, getScrollAnchor, restoreScrollAnchor]);

  const setZoom = useCallback(async (value: number) => {
    zoomModeRef.current = 'custom';
    zoomRef.current = Math.max(0.25, Math.min(4, value));
    onZoomChange?.(zoomRef.current);
    const anchor = getScrollAnchor();
    await buildLayout();
    if (anchor) restoreScrollAnchor(anchor);
    updateVisiblePages();
  }, [buildLayout, updateVisiblePages, onZoomChange, getScrollAnchor, restoreScrollAnchor]);

  const rotateCw = useCallback(async () => {
    pagesRotationRef.current = (pagesRotationRef.current + 90) % 360;
    const anchor = getScrollAnchor();
    await buildLayout();
    if (anchor) restoreScrollAnchor(anchor);
    updateVisiblePages();
  }, [buildLayout, updateVisiblePages, getScrollAnchor, restoreScrollAnchor]);

  const rotateCcw = useCallback(async () => {
    pagesRotationRef.current = (pagesRotationRef.current - 90 + 360) % 360;
    const anchor = getScrollAnchor();
    await buildLayout();
    if (anchor) restoreScrollAnchor(anchor);
    updateVisiblePages();
  }, [buildLayout, updateVisiblePages, getScrollAnchor, restoreScrollAnchor]);

  // Expose controls to parent
  useImperativeHandle(ref, () => ({
    zoomIn: () => doZoom(0.25),
    zoomOut: () => doZoom(-0.25),
    jumpToPage: (page: number) => {
      const wrapper = wrapperMapRef.current.get(page);
      if (wrapper) wrapper.scrollIntoView({ behavior: 'smooth', block: 'start' });
    },
    detectRegions: async (pageNum: number): Promise<PageRegionResult | null> => {
      const doc = pdfDocRef.current;
      if (!doc) return null;
      try {
        const page = await doc.getPage(pageNum);
        const vp = page.getViewport({ scale: 1 });
        const tc = await page.getTextContent();
        if (!tc?.items?.length) return null;

        const result = detectPageRegions(
          tc.items, pageNum, vp.width, vp.height, doc.numPages || 1,
        );
        console.log(
          '[detectRegions] rule-based:', result.regions.length,
          'regions, avg confidence', result.avgConfidence.toFixed(2),
        );
        return result;
      } catch (e: any) {
        console.error('[detectRegions] error:', e);
        throw e;
      }
    },
    refineWithLlm: async (pageNum: number): Promise<PageRegionResult | null> => {
      const doc = pdfDocRef.current;
      if (!doc) return null;
      try {
        const page = await doc.getPage(pageNum);
        const vp = page.getViewport({ scale: 1 });
        const tc = await page.getTextContent();
        if (!tc?.items?.length) return null;

        const items: LlmRegionRequest['items'] = [];
        for (const item of tc.items) {
          const s: string = item.str;
          if (!s || !s.trim()) continue;
          const t: number[] = item.transform;
          items.push({
            str: s,
            x: t[4],
            y: vp.height - t[5],
            fontSize: item.height ?? Math.abs(t[3]) ?? 12,
            fontName: item.fontName ?? '',
            width: item.width ?? 0,
          });
        }

        const request: LlmRegionRequest = {
          page: pageNum,
          pageWidth: vp.width,
          pageHeight: vp.height,
          totalPages: doc.numPages || 1,
          items,
        };

        const llmRegions = await detectRegionsLlm(request);
        console.log('[refineWithLlm] LLM returned', llmRegions.length, 'regions');
        return {
          pageIndex: pageNum,
          regions: llmRegions,
          pageWidth: vp.width,
          pageHeight: vp.height,
          avgConfidence: 1.0, // LLM results are considered high-confidence
        };
      } catch (e: any) {
        console.error('[refineWithLlm] error:', e);
        throw e;
      }
    },
    getPageTextContent: async (pageNum: number) => {
      const doc = pdfDocRef.current;
      if (!doc) return null;
      try {
        const page = await doc.getPage(pageNum);
        const vp = page.getViewport({ scale: 1 });
        const tc = await page.getTextContent();
        return {
          items: tc?.items || [],
          width: vp.width,
          height: vp.height,
        };
      } catch (e) {
        console.error('[getPageTextContent] error:', e);
        return null;
      }
    },
    getDocument: () => pdfDocRef.current,
    // Locate a verbatim quote in a page's text layer and return highlight
    // rects. Probes the hinted page first, then its neighbors (citation page
    // hints can be off by one at page boundaries).
    locateQuote: async (pageNum: number, exact: string) => {
      // The reader route may fire this right after navigation, before the
      // document finished loading — wait briefly.
      const start = Date.now();
      while (!pdfDocRef.current && Date.now() - start < 8000) {
        await new Promise((r) => setTimeout(r, 100));
      }
      const doc = pdfDocRef.current;
      if (!doc || !exact.trim()) return null;

      const waitForTextLayer = async (pn: number) => {
        const t0 = Date.now();
        while (Date.now() - t0 < 5000) {
          const tl = textLayerMapRef.current.get(pn);
          if (tl?.textContentItemsStr?.length) return tl;
          await new Promise((r) => setTimeout(r, 100));
        }
        return null;
      };

      const candidates = [pageNum, pageNum + 1, pageNum - 1]
        .filter((p, i, a) => p >= 1 && p <= doc.numPages && a.indexOf(p) === i);

      for (const pn of candidates) {
        const wrapper = wrapperMapRef.current.get(pn);
        if (!wrapper) continue;
        // Pages render lazily on visibility; scrolling there triggers it.
        wrapper.scrollIntoView({ behavior: 'auto', block: 'start' });
        const tl = await waitForTextLayer(pn);
        if (!tl) continue;

        const items: string[] = tl.textContentItemsStr;
        const joined = items.join('');
        // Whitespace-insensitive matching with an index map back to the raw
        // string: line breaks and hyphenation differ between agent-side
        // chunk text and the pdf.js text layer.
        const normToRaw: number[] = [];
        let norm = '';
        for (let i = 0; i < joined.length; i++) {
          if (!/\s/.test(joined[i])) {
            normToRaw.push(i);
            norm += joined[i];
          }
        }
        const needle = exact.replace(/\s+/g, '');
        let pos = norm.indexOf(needle);
        if (pos < 0 && needle.length > 40) {
          // LLM quotes are not always perfectly verbatim: fall back to
          // anchoring on the head of the quote.
          pos = norm.indexOf(needle.slice(0, 40));
        }
        if (pos < 0 || normToRaw.length === 0) continue;
        const rawStart = normToRaw[pos];
        const rawEnd = normToRaw[Math.min(pos + needle.length - 1, normToRaw.length - 1)] + 1;

        const wrapperRect = wrapper.getBoundingClientRect();
        const rects: SnippetRect[] = [];
        const textDivs = tl.textDivs as HTMLElement[];
        let acc = 0;
        for (let i = 0; i < items.length; i++) {
          const itemStart = acc;
          acc += items[i].length;
          if (acc <= rawStart || itemStart >= rawEnd) continue;
          const div = textDivs[i];
          if (!(div instanceof HTMLElement)) continue;
          const r = div.getBoundingClientRect();
          if (r.width <= 0 || r.height <= 0) continue;
          rects.push({
            xRatio: Math.max(0, Math.min(1, (r.left + r.width / 2 - wrapperRect.left) / wrapperRect.width)),
            yRatio: Math.max(0, Math.min(1, (r.top + r.height / 2 - wrapperRect.top) / wrapperRect.height)),
            widthRatio: Math.max(0, Math.min(1, r.width / wrapperRect.width)),
            heightRatio: Math.max(0, Math.min(1, r.height / wrapperRect.height)),
          });
        }
        if (rects.length > 0) {
          rects.sort((a, b) => a.yRatio - b.yRatio || a.xRatio - b.xRatio);
          return { pageIndex: pn, rects };
        }
      }
      return null;
    },
    setZoomMode,
    setZoom,
    rotateCw,
    rotateCcw,
  }), [doZoom, setZoomMode, setZoom, rotateCw, rotateCcw]);

  const overlayRef = useRef<HTMLDivElement[]>([]);

  // ── Snippet highlight overlay ──
  useEffect(() => {
    if (!highlightTarget || highlightTarget.length === 0) return;

    const first = highlightTarget[0];

    // Jump to the first target's page
    const wrapper = wrapperMapRef.current.get(first.pageIndex);
    if (wrapper) {
      wrapper.scrollIntoView({ behavior: 'smooth', block: 'center' });
    }

    // Wait a tick for scroll + render, then show overlays on every target
    // page that is currently rendered.
    const showTimer = setTimeout(() => {
      // Remove previous overlays wherever they were
      for (const el of overlayRef.current) el.remove();
      overlayRef.current = [];

      for (const { pageIndex, rects } of highlightTarget) {
        const w = wrapperMapRef.current.get(pageIndex);
        if (!w) continue;

        const wRect = w.getBoundingClientRect();
        const group = document.createElement('div');
        group.className = 'snippet-highlight';
        group.style.cssText = `
          position: absolute;
          inset: 0;
          pointer-events: none;
          z-index: 1;
        `;
        for (const r of rects) {
          const box = document.createElement('div');
          box.style.cssText = `
            position: absolute;
            left: ${Math.max(0, (r.xRatio - r.widthRatio / 2) * wRect.width)}px;
            top: ${Math.max(0, (r.yRatio - r.heightRatio / 2) * wRect.height)}px;
            width: ${Math.max(r.widthRatio * wRect.width, 4)}px;
            height: ${Math.max(r.heightRatio * wRect.height, 4)}px;
            background: rgba(59, 130, 246, 0.22);
            border-radius: 4px;
            transition: opacity 0.6s ease;
          `;
          group.appendChild(box);
        }
        w.appendChild(group);
        overlayRef.current.push(group);
      }

    }, 300);

    return () => {
      clearTimeout(showTimer);
    };
  }, [highlightTarget]);

  // ── Region detection overlays ──
  useEffect(() => {
    // Group regions by page
    const byPage = new Map<number, DetectedRegion[]>();
    if (regionOverlays) {
      for (const r of regionOverlays) {
        const list = byPage.get(r.pageIndex) || [];
        list.push(r);
        byPage.set(r.pageIndex, list);
      }
    }

    const cleanups: (() => void)[] = [];

    for (const [pageNum, regions] of byPage) {
      const wrapper = wrapperMapRef.current.get(pageNum);
      if (!wrapper) continue;
      cleanups.push(createRegionOverlays(wrapper, regions));
    }

    // Also clean up wrappers that no longer have regions
    for (const wrapper of wrapperMapRef.current.values()) {
      const pn = parseInt(wrapper.dataset.pageNum || '0', 10);
      if (!byPage.has(pn)) {
        cleanupRegionOverlays(wrapper);
      }
    }

    return () => cleanups.forEach(fn => fn());
  }, [regionOverlays, layoutVersion]);

  // ── Render ──
  if (loading) return (
    <div className="flex items-center justify-center h-96 text-text-secondary">
      <Loader2 size={24} className="animate-spin mr-2" />加载 PDF...
    </div>
  );
  if (error) return (
    <div className="flex items-center justify-center h-96 text-red-400 text-sm">PDF 加载失败: {error}</div>
  );

  return (
    <div ref={containerRef} className="flex flex-col bg-background overflow-hidden h-full relative">
      {/* Find bar lives outside the scroll container so it stays pinned to
          the top-right while pages scroll underneath. */}
      {searchOpen && (
        <FindBar
          query={searchQuery}
          onQueryChange={handleSearchQueryChange}
          onNext={() => navigateSearch(1)}
          onPrev={() => navigateSearch(-1)}
          onClose={closeSearch}
          matchIndex={searchMatchIndex}
          totalMatches={searchTotalMatches}
          caseSensitive={caseSensitive}
          onCaseSensitiveChange={handleCaseSensitiveChange}
        />
      )}
      {/* Scrollable page container */}
      <div
        ref={scrollRef}
        className="flex-1 overflow-auto bg-background flex flex-col items-center relative"
        tabIndex={0}
      >
      </div>
      {passwordNeeded && (
        <PasswordPrompt
          error={passwordError}
          onSubmit={(pw) => {
            setPasswordError(null);
            onPasswordSubmit?.(pw);
          }}
        />
      )}
    </div>
  );
});

/** Normalize PDF-selected text: collapse layout line-breaks into
 *  readable prose while preserving intentional paragraph boundaries.
 *
 *  Rules:
 *    1. Hyphenation break: "text-\nmore" → "textmore"
 *    2. Single newline: "line\nline" → "line line" (line-wrap collapse)
 *    3. Double newline: kept as paragraph separator
 */
export function normalizePdfSelection(text: string): string {
  // Rule 1: hyphenation across lines
  let result = text.replace(/-\n/g, '');
  // Rule 2: single newlines → space (not touching double-newlines)
  result = result.replace(/([^\n])\n([^\n])/g, '$1 $2');
  // Rule 3: collapse multiple spaces
  result = result.replace(/  +/g, ' ');
  return result.trim();
}
