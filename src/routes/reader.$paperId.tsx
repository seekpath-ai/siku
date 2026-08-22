import { createRoute } from '@tanstack/react-router';
import { Route as RootRoute } from './__root';
import { usePaper } from '@/hooks/useLibrary';
import { useTabStore } from '@/stores/tabStore';
import { usePetContextStore } from '@/stores/petContextStore';
import { useDialog } from '@/hooks/useDialog';
import { useState, useEffect, useRef, useMemo } from 'react';
import {
  Loader2, FileQuestion, ArrowLeft, ZoomIn, ZoomOut, StickyNote, FileText,
  ScanSearch, Download, Upload, MoreVertical, Pencil, Highlighter, Eraser,
  BookOpen, LayoutGrid, RotateCw, RotateCcw, Printer, Check,
} from 'lucide-react';
import { PdfViewer } from '@/components/reader/PdfViewer';
import type { PdfViewerHandle, TextSelection, PdfViewerProps, SnippetRect } from '@/components/reader/PdfViewer';
import { PdfOutline } from '@/components/reader/PdfOutline';
import { PdfThumbnails } from '@/components/reader/PdfThumbnails';
import { ExportLayoutDialog } from '@/components/reader/ExportLayoutDialog';
import { ImportRegionsDialog } from '@/components/reader/ImportRegionsDialog';
import { RegionRangeDialog } from '@/components/reader/RegionRangeDialog';
import { SelectionToolbar } from '@/components/reader/SelectionToolbar';
import { ThemePicker } from '@/components/reader/ThemePicker';
import {
  PRESET_THEMES, loadCustomThemes, saveCustomThemes,
  loadSelectedThemeId, saveSelectedThemeId, themeToPageColors,
  type ReaderTheme,
} from '@/components/reader/themes';
import { SnippetPanel } from '@/components/reader/SnippetPanel';
import { NotesTab } from '@/components/library/PaperDetailPanel';
import { useSnippetStore } from '@/stores/snippetStore';
import { useReaderStore } from '@/stores/readerStore';
import { useEvidenceStore } from '@/stores/evidenceStore';
import { useTranslationStore } from '@/stores/translationStore';
import { useTranslationStreamStore } from '@/stores/translationStreamStore';
import {
  readPdfBytes, translateTextStream, annotationUpdateTranslation, paperRecordRead,
  exportPdf, openPaperInSystem,
} from '@/lib/tauri';
import type { DetectedRegion } from '@/components/reader/regions';
import { saveCachedRegions, loadCachedRegions, clearPaperCache } from '@/lib/regionCache';
import type { DrawingTool, Stroke } from '@/components/reader/drawing';
import { DRAWING_COLORS, DEFAULT_PEN_COLOR, DEFAULT_HIGHLIGHTER_COLOR } from '@/components/reader/drawing';

type ZoomMode = 'fit-width' | 'fit-page' | 'actual' | 'custom';
type LeftSidebarMode = 'outline' | 'thumbnails' | null;

const ZOOM_MODE_LABELS: Record<ZoomMode, string> = {
  'fit-width': '适应宽度',
  'fit-page': '适应页面',
  'actual': '实际大小',
  'custom': '自定义',
};

function ReaderPage() {
  const { paperId } = Route.useParams();
  const navigate = Route.useNavigate();
  const { data: paper, isLoading: paperLoading } = usePaper(paperId);

  // Expose the current paper to the global pet.
  useEffect(() => {
    if (paper) {
      usePetContextStore.getState().setContext({
        page: 'reader',
        objectId: paper.id,
        title: paper.title || '未命名文献',
      });
    } else {
      usePetContextStore.getState().setContext(null);
    }
    return () => usePetContextStore.getState().setContext(null);
  }, [paper]);

  // Record that this paper was opened for the "最近阅读" view.
  useEffect(() => {
    if (paper) {
      paperRecordRead(paper.id).catch(() => {});
    }
  }, [paper]);

  const [pdfUrl, setPdfUrl] = useState<string | null>(null);
  const savedReaderState = useReaderStore((s) => s.getState(paperId));
  const setReaderState = useReaderStore((s) => s.setState);
  const [currentPage, setCurrentPage] = useState(savedReaderState.page);
  const [pdfLoading, setPdfLoading] = useState(false);
  const [totalPages, setTotalPages] = useState(0);
  const [displayZoom, setDisplayZoom] = useState(savedReaderState.zoom);
  const [panel, setPanel] = useState<'zhisi' | 'notes' | null>(null);
  const [panelWidth, setPanelWidth] = useState(300);
  const [toolbarSelection, setToolbarSelection] = useState<TextSelection | null>(null);
  const [highlightTarget, setHighlightTarget] = useState<PdfViewerProps['highlightTarget']>(null);
  const [clearSelectionSignal, setClearSelectionSignal] = useState(0);
  const [showRegions, setShowRegions] = useState(false);
  const [detectedRegions, setDetectedRegions] = useState<DetectedRegion[] | null>(null);
  const [regionDetecting, setRegionDetecting] = useState(false);
  const [regionDetectError, setRegionDetectError] = useState<string | null>(null);
  const regionCacheRef = useRef<Map<number, DetectedRegion[]>>(new Map());
  const [showExportDialog, setShowExportDialog] = useState(false);
  const [showImportDialog, setShowImportDialog] = useState(false);
  const [showRegionRangeDialog, setShowRegionRangeDialog] = useState(false);
  const [regionDetectProgress, setRegionDetectProgress] = useState('');
  const [moreOpen, setMoreOpen] = useState(false);
  const moreRef = useRef<HTMLDivElement>(null);
  const [drawingTool, setDrawingTool] = useState<DrawingTool | null>(null);
  const [drawingColor, setDrawingColor] = useState(DEFAULT_PEN_COLOR);
  const { confirm } = useDialog();

  // Left sidebar state
  const [leftSidebar, setLeftSidebar] = useState<LeftSidebarMode>(null);
  const [leftSidebarWidth, setLeftSidebarWidth] = useState(240);

  // Zoom/rotate state
  const [zoomMode, setZoomMode] = useState<ZoomMode>('fit-width');
  const [zoomMenuOpen, setZoomMenuOpen] = useState(false);
  const zoomMenuRef = useRef<HTMLDivElement>(null);

  // Password state
  const [password, setPassword] = useState<string | undefined>(undefined);

  // Page theme state (app-wide, persisted to localStorage)
  const [themeId, setThemeId] = useState<string>(loadSelectedThemeId);
  const [customThemes, setCustomThemes] = useState<ReaderTheme[]>(loadCustomThemes);
  const activeTheme =
    [...PRESET_THEMES, ...customThemes].find((t) => t.id === themeId) ?? PRESET_THEMES[0];
  // Memoize so PdfViewer's pageTheme prop keeps a stable reference across
  // unrelated re-renders (e.g. text selection state changes).
  const pageTheme = useMemo(() => themeToPageColors(activeTheme), [activeTheme]);

  useEffect(() => { saveSelectedThemeId(themeId); }, [themeId]);
  useEffect(() => { saveCustomThemes(customThemes); }, [customThemes]);

  const handleAddCustomTheme = (draft: { name: string; background: string; foreground: string }) => {
    const theme: ReaderTheme = { id: `custom-${Date.now()}`, custom: true, ...draft };
    setCustomThemes((list) => [...list, theme]);
    setThemeId(theme.id);
  };

  const handleDeleteCustomTheme = (id: string) => {
    setCustomThemes((list) => list.filter((t) => t.id !== id));
    if (themeId === id) setThemeId(PRESET_THEMES[0].id);
  };

  // Loaded PDF document (for sidebars)
  const [pdfDoc, setPdfDoc] = useState<any>(null);

  // Patch the pet context with the current page / selected text so the
  // literature agent can act on a specific paragraph.
  useEffect(() => {
    const cur = usePetContextStore.getState().context;
    if (cur && cur.page === 'reader' && cur.objectId === paperId) {
      usePetContextStore.getState().setContext({
        ...cur,
        pageNum: currentPage,
        selectedText: toolbarSelection?.text || cur.selectedText,
      });
    }
  }, [currentPage, toolbarSelection, paperId]);

  const drawingStorageKey = `siku.reader.strokes.${paperId}`;

  // Which paper the strokes state currently belongs to. TanStack Router
  // reuses this component when navigating between /reader/A and /reader/B
  // (same route, different params), so the useState initializer runs only
  // once — we must detect paperId changes here and reload that paper's
  // strokes, otherwise the previous paper's strokes leak into the new one.
  const strokesPaperRef = useRef(paperId);

  // Initialize strokes lazily from localStorage. Doing this in the useState
  // initializer (instead of a load effect) avoids a mount race where the
  // save effect below would overwrite the stored strokes with [] before the
  // loaded value is applied.
  const [strokes, setStrokes] = useState<Stroke[]>(() => {
    try {
      const raw = localStorage.getItem(drawingStorageKey);
      if (raw) return JSON.parse(raw) as Stroke[];
    } catch { /* ignore corrupt data */ }
    return [];
  });

  // Combined load/save: when the paper changes, load the new paper's strokes
  // (and skip saving the stale ones); otherwise persist on every change.
  useEffect(() => {
    if (strokesPaperRef.current !== paperId) {
      strokesPaperRef.current = paperId;
      try {
        const raw = localStorage.getItem(drawingStorageKey);
        setStrokes(raw ? (JSON.parse(raw) as Stroke[]) : []);
      } catch {
        setStrokes([]);
      }
      return; // do not write the previous paper's strokes into this key
    }
    try {
      localStorage.setItem(drawingStorageKey, JSON.stringify(strokes));
    } catch { /* ignore storage errors */ }
  }, [strokes, drawingStorageKey, paperId]);

  // Close the "more" dropdown when clicking outside.
  useEffect(() => {
    if (!moreOpen) return;
    const onClick = (e: MouseEvent) => {
      if (moreRef.current && !moreRef.current.contains(e.target as Node)) {
        setMoreOpen(false);
      }
    };
    document.addEventListener('click', onClick, true);
    return () => document.removeEventListener('click', onClick, true);
  }, [moreOpen]);

  // Close zoom menu when clicking outside.
  useEffect(() => {
    if (!zoomMenuOpen) return;
    const onClick = (e: MouseEvent) => {
      if (zoomMenuRef.current && !zoomMenuRef.current.contains(e.target as Node)) {
        setZoomMenuOpen(false);
      }
    };
    document.addEventListener('click', onClick, true);
    return () => document.removeEventListener('click', onClick, true);
  }, [zoomMenuOpen]);

  const handleJumpToSnippet = (snippet: { pageIndex: number; yRatio: number; xRatio: number; heightRatio: number; widthRatio: number; rects?: SnippetRect[]; segments?: { pageIndex: number; rects: SnippetRect[] }[] }) => {
    // Multi-segment snippets highlight every segment on its own page.
    if (snippet.segments && snippet.segments.length > 0) {
      setHighlightTarget(snippet.segments.map((s) => ({ pageIndex: s.pageIndex, rects: s.rects })));
    } else {
      const rects = snippet.rects && snippet.rects.length > 0
        ? snippet.rects
        : [{ xRatio: snippet.xRatio, yRatio: snippet.yRatio, widthRatio: snippet.widthRatio, heightRatio: snippet.heightRatio }];
      setHighlightTarget([{ pageIndex: snippet.pageIndex, rects }]);
    }
    pdfViewerRef.current?.jumpToPage(snippet.pageIndex);
  };
  const pdfViewerRef = useRef<PdfViewerHandle>(null);
  const blobUrlRef = useRef<string | null>(null);
  const addSnippet = useSnippetStore((s) => s.addSnippet);

  // Evidence citation requests from the pet panel or from note deep links:
  // locate the quoted passage in the text layer and paint the highlight
  // overlay (falling back to a plain page jump when the quote cannot be
  // matched). Gated on pdfDoc: on a fresh navigation the PdfViewer (and its
  // ref) only mounts after the document loads.
  const evidenceReq = useEvidenceStore((s) => s.request);
  useEffect(() => {
    if (!evidenceReq || evidenceReq.paperId !== paperId || !pdfDoc) return;
    let cancelled = false;
    (async () => {
      const hit = await pdfViewerRef.current?.locateQuote(evidenceReq.page ?? 1, evidenceReq.exact);
      if (cancelled) return;
      if (hit) {
        setHighlightTarget([hit]);
      } else if (evidenceReq.page) {
        pdfViewerRef.current?.jumpToPage(evidenceReq.page);
      }
    })();
    return () => { cancelled = true; };
  }, [evidenceReq, paperId, pdfDoc]);

  const handleTextSelect = (sel: TextSelection) => {
    setToolbarSelection(sel);
  };

  const handleToolbarDismiss = () => {
    setToolbarSelection(null);
    window.getSelection()?.removeAllRanges();
    setClearSelectionSignal((v) => v + 1);
  };

  const handleCopy = async (text: string) => {
    setToolbarSelection(null);
    try { await navigator.clipboard.writeText(text); } catch { /* ignore */ }
  };

  const handleSnippet = (sel: TextSelection) => {
    addSnippet({
      paperId,
      pageIndex: sel.pageIndex,
      yRatio: sel.yRatio,
      xRatio: sel.xRatio,
      heightRatio: sel.heightRatio,
      widthRatio: sel.widthRatio,
      rects: sel.rects,
      segments: sel.segments.map((s) => ({
        pageIndex: s.pageIndex, rects: s.rects, text: s.text, quote: s.quote,
      })),
      text: sel.text,
    });
    setPanel('zhisi');
  };

  // Translate selected text: create a snippet card, auto-translate it, and
  // stream the result into the card's translation display area.
  const handleTranslateSelection = async (sel: TextSelection) => {
    const id = addSnippet({
      paperId,
      pageIndex: sel.pageIndex,
      yRatio: sel.yRatio,
      xRatio: sel.xRatio,
      heightRatio: sel.heightRatio,
      widthRatio: sel.widthRatio,
      rects: sel.rects,
      segments: sel.segments.map((s) => ({
        pageIndex: s.pageIndex, rects: s.rects, text: s.text, quote: s.quote,
      })),
      text: sel.text,
    });
    setPanel('zhisi');
    const { targetLang } = useTranslationStore.getState();
    const stream = useTranslationStreamStore.getState();
    stream.begin(id);
    // Scroll the new card into view once the panel has rendered it.
    requestAnimationFrame(() => {
      requestAnimationFrame(() => {
        document.querySelector(`[data-snippet-id="${id}"]`)?.scrollIntoView({ behavior: 'smooth', block: 'center' });
      });
    });
    try {
      const result = await translateTextStream(sel.text, null, targetLang, (delta) => {
        useTranslationStreamStore.getState().append(id, delta);
      });
      useSnippetStore.getState().updateTranslation(id, result);
      stream.finish(id);
      // Persist to backend; retry briefly in case the snippet annotation is
      // still being created by the panel's persist effect.
      for (let i = 0; i < 5; i++) {
        try { await annotationUpdateTranslation(id, result); break; }
        catch { await new Promise((r) => setTimeout(r, 250)); }
      }
    } catch (e) {
      stream.fail(id, e instanceof Error ? e.message : String(e));
      // Translation failed — the snippet itself is still saved.
    }
  };

  // ── Region detection ──
  const handleToggleRegions = () => {
    if (showRegions) {
      setShowRegions(false);
      setDetectedRegions(null);
      regionCacheRef.current.clear();
      clearPaperCache(paperId);
      return;
    }
    // Open range dialog instead of detecting current page immediately.
    setShowRegionRangeDialog(true);
  };

  const handleDetectRange = async (startPage: number, endPage: number) => {
    setRegionDetecting(true);
    setRegionDetectError(null);
    setRegionDetectProgress('');
    setShowRegions(true);
    try {
      for (let p = startPage; p <= endPage; p++) {
        // Skip cached pages
        if (regionCacheRef.current.has(p)) continue;
        const persisted = loadCachedRegions(paperId, p);
        if (persisted) {
          regionCacheRef.current.set(p, persisted.regions);
          continue;
        }
        setRegionDetectProgress(`${p}/${endPage}`);
        const result = await pdfViewerRef.current?.detectRegions(p);
        if (result && result.regions.length > 0) {
          regionCacheRef.current.set(p, result.regions);
          saveCachedRegions(paperId, p, result.regions, 'rule');
        }
      }
      // Show regions for current page
      const cur = regionCacheRef.current.get(currentPage);
      if (cur) {
        setDetectedRegions(cur);
      } else {
        setRegionDetectError('选定范围内未检测到结构区域');
        setTimeout(() => setRegionDetectError(null), 3000);
      }
      // Low confidence on current page → offer LLM refinement
      const curResult = regionCacheRef.current.get(currentPage);
      if (curResult) {
        const avgConfidence = curResult.reduce((s, r) => s + r.confidence, 0) / curResult.length;
        if (avgConfidence < 0.55) {
          const ok = await confirm(
            `当前页结构检测置信度为 ${(avgConfidence * 100).toFixed(0)}%，规则识别结果可能不够准确。是否使用 LLM 进行精修？`,
            '检测置信度较低'
          );
          if (ok) handleLlmRefine();
        }
      }
    } catch (e: unknown) {
      setRegionDetectError(`检测失败: ${e instanceof Error ? e.message : String(e)}`);
      setTimeout(() => setRegionDetectError(null), 3000);
    } finally {
      setRegionDetecting(false);
      setRegionDetectProgress('');
      setShowRegionRangeDialog(false);
    }
  };

  // LLM refinement (user-confirmed)
  const handleLlmRefine = async () => {
    const page = currentPage;
    try {
      const result = await pdfViewerRef.current?.refineWithLlm(page);
      if (result && result.regions.length > 0) {
        regionCacheRef.current.set(page, result.regions);
        saveCachedRegions(paperId, page, result.regions, 'llm');
        setDetectedRegions(result.regions);
      }
    } catch (e: unknown) {
      setRegionDetectError(`LLM 精修失败: ${e instanceof Error ? e.message : String(e)}`);
      setTimeout(() => setRegionDetectError(null), 3000);
    }
  };

  // When the user flips pages, show cached regions for the new page.
  // If the new page has not been detected, clear the overlay instead of
  // auto-detecting (user explicitly controls the detection range).
  useEffect(() => {
    if (!showRegions) return;
    const cached = regionCacheRef.current.get(currentPage);
    setDetectedRegions(cached ?? null);
  }, [currentPage, showRegions]);

  // ── Resize handle ──
  const resizeStartXRef = useRef(0);
  const resizeStartWidthRef = useRef(0);
  const isResizingRef = useRef(false);
  const layoutRef = useRef<HTMLDivElement>(null);
  // Max panel width captured at drag start: half the app width, so the
  // snippet panel can grow to be as wide as the document (50/50 split).
  const maxPanelWidthRef = useRef(0);

  const onResizeMouseDown = (e: React.MouseEvent) => {
    e.preventDefault();
    isResizingRef.current = true;
    resizeStartXRef.current = e.clientX;
    resizeStartWidthRef.current = panelWidth;
    maxPanelWidthRef.current = Math.floor((layoutRef.current?.clientWidth ?? window.innerWidth) / 2);
  };

  // Left sidebar resize
  const leftResizeStartXRef = useRef(0);
  const leftResizeStartWidthRef = useRef(0);
  const isLeftResizingRef = useRef(false);

  const onLeftResizeMouseDown = (e: React.MouseEvent) => {
    e.preventDefault();
    isLeftResizingRef.current = true;
    leftResizeStartXRef.current = e.clientX;
    leftResizeStartWidthRef.current = leftSidebarWidth;
  };

  useEffect(() => {
    const onMouseMove = (e: MouseEvent) => {
      if (isResizingRef.current) {
        const delta = resizeStartXRef.current - e.clientX;
        setPanelWidth(Math.max(200, Math.min(maxPanelWidthRef.current || 600, resizeStartWidthRef.current + delta)));
      }
      if (isLeftResizingRef.current) {
        const delta = e.clientX - leftResizeStartXRef.current;
        setLeftSidebarWidth(Math.max(180, Math.min(400, leftResizeStartWidthRef.current + delta)));
      }
    };
    const onMouseUp = () => {
      isResizingRef.current = false;
      isLeftResizingRef.current = false;
    };
    window.addEventListener('mousemove', onMouseMove);
    window.addEventListener('mouseup', onMouseUp);
    return () => {
      window.removeEventListener('mousemove', onMouseMove);
      window.removeEventListener('mouseup', onMouseUp);
    };
  }, []);

  // Register tab on mount (once per paperId)
  useEffect(() => {
    useTabStore.getState().open({
      id: `reader-${paperId}`,
      title: '加载中...',
      icon: 'pdf',
      route: '/reader/$paperId',
      params: { paperId },
    });
  }, [paperId]);

  // Update tab title when paper loads (won't re-create if tab was closed)
  useEffect(() => {
    if (paper?.title) {
      const title = paper.title.length > 30 ? paper.title.slice(0, 30) + '...' : paper.title;
      useTabStore.getState().updateTab(`reader-${paperId}`, { title });
    }
  }, [paper?.title, paperId]);

  // Persist reader position/zoom so switching tabs doesn't reset to page 1.
  useEffect(() => {
    setReaderState(paperId, { page: currentPage });
  }, [currentPage, paperId, setReaderState]);

  useEffect(() => {
    setReaderState(paperId, { zoom: displayZoom });
  }, [displayZoom, paperId, setReaderState]);

  useEffect(() => {
    if (!paper?.id) return;
    let cancelled = false;
    setPdfLoading(true);
    setPassword(undefined);
    (async () => {
      try {
        const url = await readPdfBytes(paper.id);
        if (cancelled) return;
        // Clean up old blob URL
        if (blobUrlRef.current) URL.revokeObjectURL(blobUrlRef.current);
        blobUrlRef.current = url;
        setPdfUrl(url);
      } catch (e: unknown) {
        console.error('Failed to read PDF:', e);
      } finally {
        if (!cancelled) setPdfLoading(false);
      }
    })();
    return () => {
      cancelled = true;
      if (blobUrlRef.current) URL.revokeObjectURL(blobUrlRef.current);
    };
  }, [paper?.id]);

  const handleZoomModeChange = (mode: ZoomMode) => {
    setZoomMode(mode);
    setZoomMenuOpen(false);
    pdfViewerRef.current?.setZoomMode(mode);
  };

  const handleDownload = async () => {
    setMoreOpen(false);
    try {
      const { save } = await import('@tauri-apps/plugin-dialog');
      const target = await save({
        defaultPath: `${paper?.title || paperId}.pdf`,
        filters: [{ name: 'PDF', extensions: ['pdf'] }],
      });
      if (!target) return;
      await exportPdf(paperId, target);
    } catch (err) {
      console.error('export pdf:', err);
    }
  };

  const handlePrint = async () => {
    setMoreOpen(false);
    try {
      await openPaperInSystem(paperId);
    } catch (err) {
      console.error('open paper in system:', err);
    }
  };

  const handlePasswordSubmit = (pw: string) => {
    setPassword(pw);
  };

  // Not found
  if (!paperLoading && !paper) {
    return (
      <div className="flex flex-col items-center justify-center py-20 text-text-secondary">
        <FileQuestion size={64} className="mb-4 text-text-secondary/40" />
        <p className="text-lg mb-2">文献未找到</p>
        <button onClick={() => navigate({ to: '/library' })}
          className="flex items-center gap-2 px-4 py-2 bg-surface border border-surface-hover rounded-lg text-sm hover:bg-surface-hover">
          <ArrowLeft size={16} />返回图书馆
        </button>
      </div>
    );
  }

  return (
    <div className="flex flex-col h-full">
      {/* Top bar — page controls + zoom */}
      <div className="flex items-center gap-2 px-3 py-1 border-b border-surface-hover bg-surface/30 shrink-0">
        <span className="text-xs text-text-primary font-medium truncate flex-1">
          {paper?.title || '加载中...'}
        </span>
        <span className="text-xs text-text-secondary tabular-nums">{currentPage} / {paper?.page_count ?? '?'}</span>

        {/* Left sidebar toggles */}
        <button
          onClick={() => setLeftSidebar((s) => (s === 'outline' ? null : 'outline'))}
          className={`p-0.5 rounded transition-colors ${
            leftSidebar === 'outline' ? 'bg-primary/10 text-primary' : 'text-text-secondary hover:bg-surface-hover'
          }`}
          title="目录"
        >
          <BookOpen size={14} />
        </button>
        <button
          onClick={() => setLeftSidebar((s) => (s === 'thumbnails' ? null : 'thumbnails'))}
          className={`p-0.5 rounded transition-colors ${
            leftSidebar === 'thumbnails' ? 'bg-primary/10 text-primary' : 'text-text-secondary hover:bg-surface-hover'
          }`}
          title="缩略图"
        >
          <LayoutGrid size={14} />
        </button>

        <button
          onClick={() => pdfViewerRef.current?.zoomOut()}
          className="p-0.5 rounded hover:bg-surface-hover text-text-secondary transition-colors"
          aria-label="缩小"
        >
          <ZoomOut size={14} />
        </button>
        <span className="text-[10px] text-text-secondary/60 w-8 text-center">{Math.round(displayZoom * 100)}%</span>
        <button
          onClick={() => pdfViewerRef.current?.zoomIn()}
          className="p-0.5 rounded hover:bg-surface-hover text-text-secondary transition-colors"
          aria-label="放大"
        >
          <ZoomIn size={14} />
        </button>

        {/* Zoom mode dropdown */}
        <div ref={zoomMenuRef} className="relative">
          <button
            onClick={() => setZoomMenuOpen((v) => !v)}
            className="flex items-center gap-1 rounded px-1.5 py-0.5 text-[10px] text-text-secondary hover:bg-surface-hover transition-colors"
          >
            {ZOOM_MODE_LABELS[zoomMode]}
          </button>
          {zoomMenuOpen && (
            <div className="absolute right-0 top-full mt-1 z-40 bg-surface border border-surface-hover rounded-lg shadow-xl py-1 min-w-[100px]">
              {(Object.keys(ZOOM_MODE_LABELS) as ZoomMode[]).map((mode) => (
                <button
                  key={mode}
                  onClick={() => handleZoomModeChange(mode)}
                  className="w-full flex items-center justify-between gap-2 px-3 py-1 text-xs text-text-secondary hover:bg-surface-hover hover:text-text-primary transition-colors"
                >
                  {ZOOM_MODE_LABELS[mode]}
                  {zoomMode === mode && <Check size={12} className="text-primary" />}
                </button>
              ))}
            </div>
          )}
        </div>

        {/* Rotation */}
        <button
          onClick={() => pdfViewerRef.current?.rotateCcw()}
          className="p-0.5 rounded hover:bg-surface-hover text-text-secondary transition-colors"
          title="向左旋转"
        >
          <RotateCcw size={14} />
        </button>
        <button
          onClick={() => pdfViewerRef.current?.rotateCw()}
          className="p-0.5 rounded hover:bg-surface-hover text-text-secondary transition-colors"
          title="向右旋转"
        >
          <RotateCw size={14} />
        </button>

        {/* Page theme picker */}
        <ThemePicker
          themeId={themeId}
          customThemes={customThemes}
          onSelect={setThemeId}
          onAddCustom={handleAddCustomTheme}
          onDeleteCustom={handleDeleteCustomTheme}
        />

        <button
          onClick={() => setPanel((p) => (p === 'zhisi' ? null : 'zhisi'))}
          className={`flex items-center gap-1 px-2 py-0.5 rounded text-xs transition-colors ${
            panel === 'zhisi' ? 'bg-primary/10 text-primary' : 'text-text-secondary hover:bg-surface-hover'
          }`}
        >
          <StickyNote size={13} />
          智思
        </button>
        <button
          onClick={() => setPanel((p) => (p === 'notes' ? null : 'notes'))}
          className={`flex items-center gap-1 px-2 py-0.5 rounded text-xs transition-colors ${
            panel === 'notes' ? 'bg-primary/10 text-primary' : 'text-text-secondary hover:bg-surface-hover'
          }`}
        >
          <FileText size={13} />
          笔记
        </button>

        {/* Drawing tools */}
        <div className="flex items-center gap-0.5">
          <button
            onClick={() => {
              setDrawingTool((t) => t === 'pen' ? null : 'pen');
              setDrawingColor(DEFAULT_PEN_COLOR);
            }}
            className={`p-1 rounded transition-colors ${
              drawingTool === 'pen' ? 'bg-primary/10 text-primary' : 'text-text-secondary hover:bg-surface-hover'
            }`}
            title="画笔"
          >
            <Pencil size={14} />
          </button>
          <button
            onClick={() => {
              setDrawingTool((t) => t === 'highlighter' ? null : 'highlighter');
              setDrawingColor(DEFAULT_HIGHLIGHTER_COLOR);
            }}
            className={`p-1 rounded transition-colors ${
              drawingTool === 'highlighter' ? 'bg-primary/10 text-primary' : 'text-text-secondary hover:bg-surface-hover'
            }`}
            title="荧光笔"
          >
            <Highlighter size={14} />
          </button>
          <button
            onClick={() => setDrawingTool((t) => t === 'eraser' ? null : 'eraser')}
            className={`p-1 rounded transition-colors ${
              drawingTool === 'eraser' ? 'bg-primary/10 text-primary' : 'text-text-secondary hover:bg-surface-hover'
            }`}
            title="橡皮擦"
          >
            <Eraser size={14} />
          </button>
          {drawingTool && drawingTool !== 'eraser' && (
            <div className="flex items-center gap-0.5 ml-1 pl-1 border-l border-surface-hover">
              {DRAWING_COLORS.map((c) => (
                <button
                  key={c}
                  onClick={() => setDrawingColor(c)}
                  className={`w-3.5 h-3.5 rounded-full border transition-transform ${
                    drawingColor === c ? 'border-text-primary scale-110' : 'border-transparent hover:scale-105'
                  }`}
                  style={{ backgroundColor: c }}
                  title={c}
                />
              ))}
            </div>
          )}
        </div>

        {/* More options dropdown */}
        <div ref={moreRef} className="relative">
          <button
            onClick={() => setMoreOpen((v) => !v)}
            className="p-1 rounded text-text-secondary hover:bg-surface-hover transition-colors"
            title="更多"
            aria-label="更多"
          >
            <MoreVertical size={16} />
          </button>
          {moreOpen && (
            <div className="absolute right-0 top-full mt-1 z-40 bg-surface border border-surface-hover rounded-lg shadow-xl py-1 min-w-[140px]">
              <button
                onClick={() => { setMoreOpen(false); handleToggleRegions(); }}
                disabled={regionDetecting}
                className={`w-full flex items-center gap-2 px-3 py-1.5 text-xs transition-colors disabled:opacity-30 ${
                  showRegions ? 'text-primary hover:bg-primary/5' : 'text-text-secondary hover:bg-surface-hover hover:text-text-primary'
                }`}
              >
                {regionDetecting ? <Loader2 size={13} className="animate-spin" /> : <ScanSearch size={13} />}
                结构
              </button>
              <button
                onClick={handleDownload}
                className="w-full flex items-center gap-2 px-3 py-1.5 text-xs text-text-secondary hover:bg-surface-hover hover:text-text-primary transition-colors"
              >
                <Download size={13} />
                下载
              </button>
              <button
                onClick={handlePrint}
                className="w-full flex items-center gap-2 px-3 py-1.5 text-xs text-text-secondary hover:bg-surface-hover hover:text-text-primary transition-colors"
              >
                <Printer size={13} />
                打印
              </button>
              <button
                onClick={() => { setMoreOpen(false); setShowExportDialog(true); }}
                className="w-full flex items-center gap-2 px-3 py-1.5 text-xs text-text-secondary hover:bg-surface-hover hover:text-text-primary transition-colors"
              >
                <Download size={13} />
                导出
              </button>
              <button
                onClick={() => { setMoreOpen(false); setShowImportDialog(true); }}
                className="w-full flex items-center gap-2 px-3 py-1.5 text-xs text-text-secondary hover:bg-surface-hover hover:text-text-primary transition-colors"
              >
                <Upload size={13} />
                导入
              </button>
            </div>
          )}
        </div>
      </div>

      {/* PDF + Sidebars + Snippet Panel */}
      <div ref={layoutRef} className="flex-1 flex min-h-0">
        {/* Left sidebar: outline or thumbnails */}
        {leftSidebar && (
          <>
            <div className="shrink-0 border-r border-surface-hover bg-surface/20" style={{ width: leftSidebarWidth }}>
              {leftSidebar === 'outline' && pdfDoc && (
                <PdfOutline
                  doc={pdfDoc}
                  onJumpToPage={(p) => pdfViewerRef.current?.jumpToPage(p)}
                />
              )}
              {leftSidebar === 'thumbnails' && pdfDoc && (
                <PdfThumbnails
                  doc={pdfDoc}
                  currentPage={currentPage}
                  onSelect={(p) => pdfViewerRef.current?.jumpToPage(p)}
                />
              )}
            </div>
            <div
              onMouseDown={onLeftResizeMouseDown}
              className="w-px shrink-0 bg-surface-hover/30 hover:bg-primary/50 active:bg-primary cursor-col-resize transition-colors"
              style={{ userSelect: 'none' }}
            />
          </>
        )}

        {/* PDF area */}
        <div className="flex-1 min-w-0 relative">
          {pdfLoading ? (
            <div className="flex items-center justify-center h-full text-text-secondary">
              <Loader2 size={24} className="animate-spin mr-2" />加载 PDF...
            </div>
          ) : pdfUrl ? (
            <>
              <div className="h-full">
                <PdfViewer
                  ref={pdfViewerRef}
                  src={pdfUrl}
                  initialPage={savedReaderState.page}
                  initialZoom={savedReaderState.zoom}
                  onPageChange={setCurrentPage}
                  onTotalPages={setTotalPages}
                  onZoomChange={setDisplayZoom}
                  onTextSelect={handleTextSelect}
                  onSelectionClear={() => setToolbarSelection(null)}
                  clearSelectionSignal={clearSelectionSignal}
                  highlightTarget={highlightTarget}
                  regionOverlays={showRegions ? detectedRegions : null}
                  drawingTool={drawingTool}
                  drawingColor={drawingColor}
                  strokes={strokes}
                  onStrokesChange={setStrokes}
                  password={password}
                  onPasswordSubmit={handlePasswordSubmit}
                  onDocumentLoaded={setPdfDoc}
                  pageTheme={pageTheme}
                />
              </div>
              {toolbarSelection && (
                <SelectionToolbar
                  selection={toolbarSelection}
                  onCopy={handleCopy}
                  onSnippet={handleSnippet}
                  onTranslate={handleTranslateSelection}
                  onDismiss={handleToolbarDismiss}
                />
              )}
              {regionDetectError && (
                <div className="absolute bottom-4 left-1/2 -translate-x-1/2 px-4 py-2 rounded-lg bg-red-500/90 text-white text-xs shadow-lg z-30">
                  {regionDetectError}
                </div>
              )}

              <ExportLayoutDialog
                isOpen={showExportDialog}
                onClose={() => setShowExportDialog(false)}
                totalPages={totalPages}
                currentPage={currentPage}
                pdfViewerRef={pdfViewerRef}
              />
              <ImportRegionsDialog
                isOpen={showImportDialog}
                onClose={() => setShowImportDialog(false)}
                totalPages={totalPages}
                currentPage={currentPage}
                onImport={(regions, pageIndex) => {
                  regionCacheRef.current.set(pageIndex, regions);
                  saveCachedRegions(paperId, pageIndex, regions, 'import');
                  setDetectedRegions(regions);
                  setShowRegions(true);
                  pdfViewerRef.current?.jumpToPage(pageIndex);
                }}
              />
              <RegionRangeDialog
                isOpen={showRegionRangeDialog}
                onClose={() => setShowRegionRangeDialog(false)}
                totalPages={totalPages}
                currentPage={currentPage}
                onDetect={handleDetectRange}
                detecting={regionDetecting}
                progress={regionDetectProgress}
              />
            </>
          ) : (
            <div className="flex items-center justify-center h-full text-text-secondary/60 text-sm">
              无 PDF 文件
            </div>
          )}
        </div>

        {/* Right panel: 智思 snippets or paper notes */}
        {panel && (
          <>
            {/* Resize handle */}
            <div
              onMouseDown={onResizeMouseDown}
              className="w-px shrink-0 bg-surface-hover/30 hover:bg-primary/50 active:bg-primary cursor-col-resize transition-colors"
              style={{ userSelect: 'none' }}
            />
            <div className="shrink-0" style={{ width: panelWidth }}>
              {panel === 'zhisi' ? (
                <SnippetPanel
                  paperId={paperId}
                  totalPages={totalPages}
                  onJumpToSnippet={handleJumpToSnippet}
                />
              ) : (
                <NotesTab paperId={paperId} />
              )}
            </div>
          </>
        )}
      </div>
    </div>
  );
}

export const Route = createRoute({
  getParentRoute: () => RootRoute,
  path: '/reader/$paperId',
  component: ReaderPage,
});
