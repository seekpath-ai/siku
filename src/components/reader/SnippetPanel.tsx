import { useMemo, useCallback, useState, useEffect, useRef } from 'react';
import { Trash2, StickyNote, ChevronRight, ChevronDown, Copy, Languages, Check, Loader2, Tag, X, Filter, Clock } from 'lucide-react';
import { useSnippetStore, Snippet } from '@/stores/snippetStore';
import { useTranslationStore, type TargetLang } from '@/stores/translationStore';
import { useTranslationStreamStore } from '@/stores/translationStreamStore';
import { translateTextStream, annotationList, annotationCreate, annotationDelete, annotationClearPaper, annotationUpdateTags, annotationUpdateNote, annotationUpdateTranslation, notesCreate, noteAddExcerpt } from '@/lib/tauri';
import type { AnnotationRow } from '@/lib/tauri';
import { isoToDisplay } from '@/lib/time';
import { ConfirmButton } from '@/components/ui/ConfirmButton';
import { useDialog } from '@/hooks/useDialog';

const LANG_LABELS: Record<TargetLang, string> = { zh: '中文', en: 'English', ja: '日本語' };

interface SnippetPanelProps {
  paperId: string;
  totalPages: number;
  onJumpToSnippet?: (snippet: Snippet) => void;
}

/** Group snippets by page index */
interface PageGroup {
  pageIndex: number;
  snippets: Snippet[];
}

// Font size for snippet body text (quote / note / translation), in px
const FONT_MIN = 10;
const FONT_MAX = 18;
const FONT_DEFAULT = 12;
const FONT_STORAGE_KEY = 'siku.zhisi.fontSize';

export function SnippetPanel({ paperId, totalPages, onJumpToSnippet }: SnippetPanelProps) {
  const { alert } = useDialog();
  const snippets = useSnippetStore((s) => s.snippets);
  const addSnippet = useSnippetStore((s) => s.addSnippet);
  const removeSnippet = useSnippetStore((s) => s.removeSnippet);
  const updateNote = useSnippetStore((s) => s.updateNote);
  const addTag = useSnippetStore((s) => s.addTag);
  const removeTag = useSnippetStore((s) => s.removeTag);
  const updateTranslation = useSnippetStore((s) => s.updateTranslation);
  const setAll = useSnippetStore((s) => s.setAll);
  const clearPaper = useSnippetStore((s) => s.clearPaper);

  // Reload the backend list (used to roll back optimistic deletes/clears).
  const reloadFromBackend = useCallback(async () => {
    const rows = await annotationList(paperId);
    const snips: Snippet[] = rows.map((r) => {
      let rect = { x: 0.5, y: 0, w: 0.5, h: 0.02 };
      try { rect = JSON.parse(r.rect); } catch { /* keep default */ }
      let tags: string[] = [];
      try { tags = JSON.parse(r.tags || '[]'); } catch { /* keep default */ }
      return {
        id: r.id, paperId: r.paper_id, pageIndex: r.page,
        xRatio: rect.x, yRatio: rect.y, widthRatio: rect.w, heightRatio: rect.h,
        text: r.text || '', note: r.note || '', tags,
        translation: r.translation,
        createdAt: r.created_at,
      };
    });
    setAll(snips);
  }, [paperId, setAll]);

  // Wrap remove/clear to sync with backend. Failures roll back the optimistic
  // update and surface an alert instead of silently losing data.
  const handleRemove = useCallback(async (id: string) => {
    const sn = useSnippetStore.getState().snippets.find((s) => s.id === id);
    removeSnippet(id);
    try {
      await annotationDelete(id);
    } catch (err) {
      console.error('删除摘录失败:', err);
      if (sn) addSnippet(sn);
      await alert('删除摘录失败，已恢复。', '删除失败');
    }
  }, [removeSnippet, addSnippet, alert]);

  // Retry helper for updates that may race with the initial annotationCreate.
  const withCreateRetry = useCallback(<T,>(
    fn: () => Promise<T>,
    label: string,
    attempts = 3,
    delayMs = 250
  ): Promise<T | undefined> => {
    return fn().catch(async (err) => {
      if (attempts > 1) {
        await new Promise((r) => setTimeout(r, delayMs));
        return withCreateRetry(fn, label, attempts - 1, delayMs);
      }
      console.error(`${label} 失败:`, err);
      return undefined;
    });
  }, []);

  // Wrap updateNote/updateTranslation to sync with backend.
  // Snippets are created asynchronously, so a note/translation update can
  // arrive before the annotation row exists. Retry briefly instead of dropping
  // the update.
  const handleUpdateNote = useCallback((id: string, note: string) => {
    updateNote(id, note);
    withCreateRetry(() => annotationUpdateNote(id, note), '保存笔记');
  }, [updateNote, withCreateRetry]);

  const handleUpdateTranslation = useCallback((id: string, translation: string | null) => {
    updateTranslation(id, translation);
    const value = translation || '';
    withCreateRetry(() => annotationUpdateTranslation(id, value), '保存译文');
  }, [updateTranslation, withCreateRetry]);

  const handleClear = useCallback(async () => {
    clearPaper(paperId);
    try {
      await annotationClearPaper(paperId);
    } catch (err) {
      console.error('清空摘录失败:', err);
      await reloadFromBackend().catch(() => {});
      await alert('清空摘录失败，已恢复原数据。', '清空失败');
    }
  }, [clearPaper, paperId, reloadFromBackend, alert]);

  // Wrap addTag/removeTag to sync with backend
  const handleAddTag = useCallback((id: string, tag: string) => {
    addTag(id, tag);
    const sn = useSnippetStore.getState().snippets.find((s) => s.id === id);
    if (sn) annotationUpdateTags(id, sn.tags).catch((err) => console.error('保存标签失败:', err));
  }, [addTag]);

  const handleRemoveTag = useCallback((id: string, tag: string) => {
    removeTag(id, tag);
    const sn = useSnippetStore.getState().snippets.find((s) => s.id === id);
    if (sn) annotationUpdateTags(id, sn.tags).catch((err) => console.error('保存标签失败:', err));
  }, [removeTag]);

  const [tagFilter, setTagFilter] = useState<string | null>(null);
  const [loaded, setLoaded] = useState(false);

  // Batch "translate all" state: progress counter and scroll target.
  // Per-card streaming text lives in translationStreamStore.
  const [batch, setBatch] = useState<{ done: number; total: number } | null>(null);
  const cardRefs = useRef<Map<string, HTMLDivElement>>(new Map());
  const registerCardRef = useCallback((id: string, el: HTMLDivElement | null) => {
    if (el) cardRefs.current.set(id, el);
    else cardRefs.current.delete(id);
  }, []);

  // Translate all untranslated snippets in place, one at a time. Each snippet
  // gets streamed into its translation display area while being translated,
  // and the list scrolls to the active card so the user sees what is running.
  const handleBatchTranslate = useCallback(async () => {
    if (batch) return;
    const items = snippets.filter((s) => s.paperId === paperId && !s.translation);
    if (items.length === 0) return;
    const lang = useTranslationStore.getState().targetLang;
    const stream = useTranslationStreamStore.getState();
    setBatch({ done: 0, total: items.length });
    for (let i = 0; i < items.length; i++) {
      const item = items[i];
      stream.begin(item.id);
      cardRefs.current.get(item.id)?.scrollIntoView({ behavior: 'smooth', block: 'center' });
      try {
        const full = await translateTextStream(item.text, null, lang, (delta) => {
          useTranslationStreamStore.getState().append(item.id, delta);
        });
        handleUpdateTranslation(item.id, full);
        useTranslationStreamStore.getState().finish(item.id);
      } catch (e) {
        useTranslationStreamStore.getState().fail(
          item.id,
          e instanceof Error ? e.message : String(e)
        );
      }
      setBatch((b) => (b ? { ...b, done: i + 1 } : b));
    }
    setBatch(null);
  }, [batch, snippets, paperId, handleUpdateTranslation]);

  const [fontSize, setFontSize] = useState<number>(() => {
    const v = Number(localStorage.getItem(FONT_STORAGE_KEY));
    return Number.isFinite(v) && v >= FONT_MIN && v <= FONT_MAX ? v : FONT_DEFAULT;
  });
  useEffect(() => {
    localStorage.setItem(FONT_STORAGE_KEY, String(fontSize));
  }, [fontSize]);

  // Load snippets from backend on mount
  useEffect(() => {
    let cancelled = false;
    annotationList(paperId).then((rows: AnnotationRow[]) => {
      if (cancelled) return;
      const snips: Snippet[] = rows.map((r) => {
        // Mark as already persisted so sync effect won't re-create
        persistedRef.current.add(r.id);
        let rect = { x: 0.5, y: 0, w: 0.5, h: 0.02 };
        try { rect = JSON.parse(r.rect); } catch { /* keep default */ }
        let tags: string[] = [];
        try { tags = JSON.parse(r.tags || '[]'); } catch { /* keep default */ }
        // rect JSON may carry per-line rects ({x,y,w,h,rects:[...]} for new
        // rows) and multi-range segments ({segments:[{page,rects,text,
        // quote}]}); legacy rows only have the single bounding rect.
        const parsed = rect as {
          x: number; y: number; w: number; h: number; rects?: unknown;
          segments?: { page: number; rects?: { x: number; y: number; w: number; h: number }[]; text?: string; quote?: { prefix: string; exact: string; suffix: string } }[];
        };
        const rects = Array.isArray(parsed.rects)
          ? (parsed.rects as { x: number; y: number; w: number; h: number }[]).map((r) => ({
              xRatio: r.x, yRatio: r.y, widthRatio: r.w, heightRatio: r.h,
            }))
          : undefined;
        const segments = Array.isArray(parsed.segments)
          ? parsed.segments.map((s) => ({
              pageIndex: s.page,
              rects: (s.rects ?? []).map((r) => ({
                xRatio: r.x, yRatio: r.y, widthRatio: r.w, heightRatio: r.h,
              })),
              text: s.text ?? '',
              quote: s.quote ?? { prefix: '', exact: s.text ?? '', suffix: '' },
            }))
          : undefined;
        return {
          id: r.id, paperId: r.paper_id, pageIndex: r.page,
          xRatio: rect.x, yRatio: rect.y, widthRatio: rect.w, heightRatio: rect.h,
          ...(rects && rects.length > 0 ? { rects } : {}),
          ...(segments && segments.length > 0 ? { segments } : {}),
          text: r.text || '', note: r.note || '', tags,
          translation: r.translation,
          createdAt: r.created_at,
        };
      });
      setAll((current) => {
        // Keep snippets added locally while the backend list was still
        // loading (e.g. "translate selection" opening the panel on a fresh
        // paper); they are persisted by the effect below.
        const rowIds = new Set(snips.map((s) => s.id));
        const extras = current.filter(
          (s) => s.paperId === paperId && !rowIds.has(s.id) && !persistedRef.current.has(s.id)
        );
        return [...extras, ...snips];
      });
      setLoaded(true);
    }).catch(() => { if (!cancelled) setLoaded(true); });
    return () => { cancelled = true; };
  }, [paperId, setAll]);

  // Persist new snippets to backend
  const persistedRef = useRef<Set<string>>(new Set());
  useEffect(() => {
    if (!loaded) return;
    for (const sn of snippets) {
      if (sn.paperId !== paperId || persistedRef.current.has(sn.id)) continue;
      persistedRef.current.add(sn.id);
      const capturedId = sn.id;
      annotationCreate(
        sn.paperId, sn.pageIndex, sn.xRatio, sn.yRatio, sn.widthRatio, sn.heightRatio,
        sn.text || null, sn.note || null, sn.tags, sn.id,
        sn.rects && sn.rects.length > 0
          ? sn.rects.map((r) => ({ x: r.xRatio, y: r.yRatio, w: r.widthRatio, h: r.heightRatio }))
          : null,
        sn.segments && sn.segments.length > 0
          ? sn.segments.map((s) => ({
              page: s.pageIndex,
              rects: s.rects.map((r) => ({ x: r.xRatio, y: r.yRatio, w: r.widthRatio, h: r.heightRatio })),
              text: s.text,
              quote: s.quote,
            }))
          : null,
      )
        .then(async () => {
          // Notes / translations may have changed while annotationCreate was
          // in flight. Sync the current store values so nothing is lost.
          const current = useSnippetStore.getState().snippets.find((s) => s.id === capturedId);
          if (!current) return;
          const syncs: Promise<unknown>[] = [];
          if (current.note) {
            syncs.push(annotationUpdateNote(current.id, current.note).catch((err) => console.error('同步摘录笔记失败:', err)));
          }
          if (current.translation) {
            syncs.push(annotationUpdateTranslation(current.id, current.translation).catch((err) => console.error('同步摘录译文失败:', err)));
          }
          await Promise.all(syncs);
        })
        .catch((err) => {
          console.error('创建摘录持久化失败:', err);
          // Allow retry on next effect run; otherwise the snippet is lost.
          persistedRef.current.delete(capturedId);
        });
    }
  }, [snippets, paperId, loaded]);

  // Collect all unique tags for this paper
  const allTags = useMemo(() => {
    const ts = new Set<string>();
    for (const s of snippets) {
      if (s.paperId === paperId) s.tags.forEach((t) => ts.add(t));
    }
    return [...ts].sort();
  }, [snippets, paperId]);

  // Filter + group by page
  const pageGroups = useMemo(() => {
    let paperSnippets = snippets.filter((s) => s.paperId === paperId);
    if (tagFilter) {
      paperSnippets = paperSnippets.filter((s) => s.tags.includes(tagFilter));
    }
    const map = new Map<number, Snippet[]>();
    for (const sn of paperSnippets) {
      const list = map.get(sn.pageIndex) || [];
      list.push(sn);
      map.set(sn.pageIndex, list);
    }
    const groups: PageGroup[] = [];
    for (const [pageIndex, list] of map) {
      groups.push({ pageIndex, snippets: list });
    }
    groups.sort((a, b) => a.pageIndex - b.pageIndex);
    return groups;
  }, [snippets, paperId, tagFilter]);

  const totalCount = snippets.filter((s) => s.paperId === paperId).length;

  // Handle paste/drop of external text (for manual snippet creation).
  // Ignore paste into editable elements (note textarea, tag input, etc.) so
  // the user can paste into a note without spawning a new snippet card.
  const handlePaste = useCallback((e: React.ClipboardEvent, pageIndex: number) => {
    const target = e.target as HTMLElement;
    if (
      target.tagName === 'TEXTAREA' ||
      target.tagName === 'INPUT' ||
      target.isContentEditable
    ) {
      return;
    }
    const text = e.clipboardData.getData('text/plain').trim();
    if (!text) return;
    e.preventDefault();
    addSnippet({ paperId, pageIndex, yRatio: 0, xRatio: 0, heightRatio: 0.02, widthRatio: 0.5, text });
  }, [paperId, addSnippet]);

  if (pageGroups.length === 0) {
    return (
      <div className="flex flex-col h-full bg-background">
        <PanelHeader count={0} filteredCount={0} tags={[]} tagFilter={null} onSetTagFilter={() => {}} onClear={() => {}} fontSize={fontSize} onFontSizeChange={setFontSize} batchActive={false} batchDone={0} batchTotal={0} onBatchTranslate={() => {}} />
        <div className="flex-1 flex items-center justify-center text-text-secondary/40 text-sm px-4 text-center">
          在左侧 PDF 中划选文字，<br />摘录将按原文页码排序显示在这里
        </div>
      </div>
    );
  }

  const maxPage = pageGroups[pageGroups.length - 1].pageIndex;

  return (
    <div className="flex flex-col h-full bg-background">
      <PanelHeader
        count={totalCount}
        filteredCount={pageGroups.reduce((acc, g) => acc + g.snippets.length, 0)}
        tags={allTags}
        tagFilter={tagFilter}
        onSetTagFilter={setTagFilter}
        onClear={handleClear}
        fontSize={fontSize}
        onFontSizeChange={setFontSize}
        batchActive={batch !== null}
        batchDone={batch?.done ?? 0}
        batchTotal={batch?.total ?? 0}
        onBatchTranslate={handleBatchTranslate}
      />

      <div className="flex-1 overflow-y-auto pl-0 pr-2 py-3 space-y-2">
        {/* Only render pages that actually have snippets. */}
        {pageGroups.map((group) => (
          <PageSection
            key={group.pageIndex}
            pageNum={group.pageIndex}
            snippets={group.snippets}
            fontSize={fontSize}
            onUpdateNote={handleUpdateNote}
            onUpdateTranslation={handleUpdateTranslation}
            onRemove={handleRemove}
            onAddTag={handleAddTag}
            onRemoveTag={handleRemoveTag}
            onJump={onJumpToSnippet}
            onPaste={(e) => handlePaste(e, group.pageIndex)}
            registerRef={registerCardRef}
          />
        ))}

        {/* Pages beyond the last annotated page */}
        {maxPage < totalPages && (
          <div className="text-[10px] text-text-secondary/20 text-center pt-3 pb-1 select-none">
            · · · 共 {totalPages} 页 · · ·
          </div>
        )}
      </div>
    </div>
  );
}

function PanelHeader({
  count, filteredCount, tags, tagFilter, onSetTagFilter, onClear, fontSize, onFontSizeChange,
  batchActive, batchDone, batchTotal, onBatchTranslate,
}: {
  count: number; filteredCount: number;
  tags: string[]; tagFilter: string | null;
  onSetTagFilter: (t: string | null) => void;
  onClear: () => void;
  fontSize: number;
  onFontSizeChange: (n: number) => void;
  batchActive: boolean;
  batchDone: number;
  batchTotal: number;
  onBatchTranslate: () => void;
}) {
  const [langOpen, setLangOpen] = useState(false);
  const langRef = useRef<HTMLDivElement>(null);
  const { targetLang, setTargetLang } = useTranslationStore();

  // Close language dropdown on outside click
  useEffect(() => {
    if (!langOpen) return;
    const onDown = (e: MouseEvent) => {
      if (langRef.current && !langRef.current.contains(e.target as Node)) setLangOpen(false);
    };
    document.addEventListener('mousedown', onDown);
    return () => document.removeEventListener('mousedown', onDown);
  }, [langOpen]);

  return (
    <div className="shrink-0">
      <div className="flex items-center gap-2 px-3 py-2 border-b border-surface-hover">
        <StickyNote size={14} className="text-primary" />
        <span className="text-xs font-medium text-text-primary">智思</span>
        {count > 0 && (
          <span className="text-[10px] text-text-secondary/60 ml-1">
            {tagFilter ? `${filteredCount}/${count}` : count} 条
          </span>
        )}
        <div className="flex-1" />
        {/* Translation target language dropdown */}
        <div className="relative" ref={langRef}>
          <button
            onClick={() => setLangOpen(!langOpen)}
            className={`flex items-center gap-1 px-1.5 py-0.5 rounded text-[10px] transition-colors ${
              langOpen ? 'bg-primary/10 text-primary' : 'text-text-secondary/50 hover:bg-surface-hover hover:text-text-primary'
            }`}
            title="翻译目标语言"
          >
            {LANG_LABELS[targetLang]}
            <ChevronDown size={10} />
          </button>
          {langOpen && (
            <div className="absolute right-0 top-full mt-1 z-30 bg-surface border border-surface-hover rounded shadow-xl py-1 min-w-[88px]">
              {(Object.keys(LANG_LABELS) as TargetLang[]).map((lang) => (
                <button
                  key={lang}
                  onClick={() => { setTargetLang(lang); setLangOpen(false); }}
                  className={`w-full text-left px-2.5 py-1 text-[11px] transition-colors ${
                    lang === targetLang ? 'text-primary' : 'text-text-secondary hover:bg-surface-hover hover:text-text-primary'
                  }`}
                >
                  {LANG_LABELS[lang]}
                </button>
              ))}
            </div>
          )}
        </div>
        {/* Font size stepper for quote / note / translation text */}
        <div className="flex items-center gap-0.5 mr-1 text-[10px] text-text-secondary/50 select-none">
          <button
            onClick={() => onFontSizeChange(fontSize - 1)}
            disabled={fontSize <= FONT_MIN}
            className="px-1 py-0.5 rounded hover:bg-surface-hover hover:text-text-primary transition-colors disabled:opacity-30 disabled:pointer-events-none"
            title="减小字号"
          >
            A−
          </button>
          <span className="w-6 text-center tabular-nums">{fontSize}</span>
          <button
            onClick={() => onFontSizeChange(fontSize + 1)}
            disabled={fontSize >= FONT_MAX}
            className="px-1 py-0.5 rounded hover:bg-surface-hover hover:text-text-primary transition-colors disabled:opacity-30 disabled:pointer-events-none"
            title="增大字号"
          >
            A+
          </button>
        </div>
        {count > 0 && (
          <>
            {batchActive ? (
              <span className="text-[10px] text-text-secondary/60 flex items-center gap-1">
                <Loader2 size={10} className="animate-spin" />{batchDone}/{batchTotal}
              </span>
            ) : (
              <button
                onClick={onBatchTranslate}
                className="text-[10px] text-text-secondary/40 hover:text-primary transition-colors flex items-center gap-1"
                title="翻译全部未翻译摘录"
              >
                <Languages size={10} />翻译全部
              </button>
            )}
            <ConfirmButton onConfirm={onClear} confirmText="确认清空？">
              清空
            </ConfirmButton>
          </>
        )}
      </div>
      {/* Tag filter bar */}
      {tags.length > 0 && (
        <div className="flex items-center gap-1 px-3 py-1.5 border-b border-surface-hover overflow-x-auto">
          <Filter size={10} className="text-text-secondary/40 shrink-0" />
          {tagFilter && (
            <button
              onClick={() => onSetTagFilter(null)}
              className="flex items-center gap-0.5 text-[10px] px-1.5 py-0.5 rounded bg-primary/10 text-primary shrink-0"
            >
              {tagFilter}<X size={10} />
            </button>
          )}
          {tags.filter((t) => t !== tagFilter).map((t) => (
            <button
              key={t}
              onClick={() => onSetTagFilter(t)}
              className="text-[10px] px-1.5 py-0.5 rounded bg-surface-hover text-text-secondary hover:bg-primary/10 hover:text-primary transition-colors shrink-0"
            >
              {t}
            </button>
          ))}
        </div>
      )}
    </div>
  );
}

function PageSection({
  pageNum, snippets, fontSize, onUpdateNote, onUpdateTranslation, onRemove, onAddTag, onRemoveTag, onJump, onPaste,
  registerRef,
}: {
  pageNum: number;
  snippets: Snippet[];
  fontSize: number;
  onUpdateNote: (id: string, note: string) => void;
  onUpdateTranslation: (id: string, translation: string | null) => void;
  onRemove: (id: string) => void;
  onAddTag: (id: string, tag: string) => void;
  onRemoveTag: (id: string, tag: string) => void;
  onJump?: (snippet: Snippet) => void;
  onPaste: (e: React.ClipboardEvent) => void;
  registerRef: (id: string, el: HTMLDivElement | null) => void;
}) {
  return (
    <div className="mb-2">
      <div className="flex items-center gap-1 w-full py-1">
        <ChevronRight size={10} className="text-primary/60" />
        <span className="text-[11px] font-medium text-text-secondary">第 {pageNum} 页</span>
      </div>
      <div className="space-y-2 pl-0">
        {snippets.map((sn) => (
          <SnippetCard
            key={sn.id}
            snippet={sn}
            fontSize={fontSize}
            onUpdateNote={onUpdateNote}
            onUpdateTranslation={onUpdateTranslation}
            onRemove={onRemove}
            onAddTag={onAddTag}
            onRemoveTag={onRemoveTag}
            onPaste={onPaste}
            onJump={onJump}
            registerRef={registerRef}
          />
        ))}
      </div>
    </div>
  );
}

function SnippetCard({
  snippet, fontSize, onUpdateNote, onUpdateTranslation, onRemove, onAddTag, onRemoveTag, onPaste, onJump,
  registerRef,
}: {
  snippet: Snippet;
  fontSize: number;
  onUpdateNote: (id: string, note: string) => void;
  onUpdateTranslation: (id: string, translation: string | null) => void;
  onRemove: (id: string) => void;
  onAddTag: (id: string, tag: string) => void;
  onRemoveTag: (id: string, tag: string) => void;
  onPaste: (e: React.ClipboardEvent) => void;
  onJump?: (snippet: Snippet) => void;
  registerRef: (id: string, el: HTMLDivElement | null) => void;
}) {
  const [copied, setCopied] = useState(false);
  const [transCopied, setTransCopied] = useState(false);
  const [tagInput, setTagInput] = useState('');
  const { alert } = useDialog();
  const translation = snippet.translation;
  const targetLang = useTranslationStore((s) => s.targetLang);

  // Text streamed so far for this card (batch "translate all", the single
  // translate button, or the reader's "translate selection" flow). Key
  // presence in the store means a stream is currently in flight.
  const streamText = useTranslationStreamStore((s) => s.texts[snippet.id]);
  const streamError = useTranslationStreamStore((s) => s.errors[snippet.id]);
  const isStreaming = streamText !== undefined;

  const handleTranslate = async () => {
    if (translation) { onUpdateTranslation(snippet.id, null); return; }
    if (isStreaming) return;
    const stream = useTranslationStreamStore.getState();
    stream.begin(snippet.id);
    try {
      const result = await translateTextStream(snippet.text, null, targetLang, (delta) => {
        useTranslationStreamStore.getState().append(snippet.id, delta);
      });
      stream.finish(snippet.id);
      onUpdateTranslation(snippet.id, result);
    } catch (e) {
      stream.fail(snippet.id, e instanceof Error ? e.message : String(e));
    }
  };

  const handleCopy = () => {
    navigator.clipboard.writeText(snippet.text);
    setCopied(true);
    setTimeout(() => setCopied(false), 1500);
  };

  const handleCopyTranslation = () => {
    if (snippet.translation) navigator.clipboard.writeText(snippet.translation);
    setTransCopied(true);
    setTimeout(() => setTransCopied(false), 1500);
  };

  const handleAddTag = () => {
    if (tagInput.trim()) {
      onAddTag(snippet.id, tagInput.trim());
      setTagInput('');
    }
  };

  const timeStr = isoToDisplay(snippet.createdAt);

  // Clicking anywhere on the card (quote, note, translation) jumps the PDF to
  // the snippet's region — except on interactive elements.
  const handleCardClick = (e: React.MouseEvent) => {
    const t = e.target as HTMLElement;
    if (t.closest('button, textarea, input, select, a')) return;
    onJump?.(snippet);
  };

  // Turn the snippet (quote + note) into an independent note in the vault.
  const handleConvertToNote = async () => {
    const title = (snippet.text || snippet.note || '摘录笔记').slice(0, 30).trim() || '摘录笔记';
    const content = [`> ${snippet.text}`, snippet.note ? `\n${snippet.note}` : ''].join('');
    try {
      // Paper snippets merge-append into the paper's excerpt note (titled with
      // the paper's title); standalone snippets create an independent note.
      const note = snippet.paperId
        ? await noteAddExcerpt(snippet.paperId, content)
        : await notesCreate(title, content);
      await alert(`已创建笔记「${note.title}」，可在笔记页查看`, '转为笔记');
    } catch (err) {
      console.error('convert to note:', err);
      await alert(`创建笔记失败：${err}`, '转为笔记');
    }
  };

  return (
    <div
      ref={(el) => registerRef(snippet.id, el)}
      data-snippet-id={snippet.id}
      onClick={handleCardClick}
      className={`bg-background rounded border p-2 group/card transition-colors cursor-pointer ${
        isStreaming ? 'border-primary/60 ring-1 ring-primary/30' : 'border-surface-hover hover:border-primary/30'
      }`}
    >
      {/* Quoted text */}
      <blockquote
        className="text-text-primary leading-relaxed mb-1.5 pl-0 whitespace-pre-wrap break-words"
        style={{ fontSize }}
      >
        {snippet.text}
      </blockquote>

      {/* Note textarea — vertically resizable */}
      <textarea
        className="w-full bg-transparent text-text-secondary placeholder:text-text-secondary/25
                   resize-y outline-none border-0 rounded
                   focus:bg-surface/50 focus:text-text-primary transition-colors
                   leading-relaxed min-h-[2.75rem]"
        style={{ fontSize }}
        rows={2}
        placeholder="添加笔记…"
        value={snippet.note}
        onChange={(e) => onUpdateNote(snippet.id, e.target.value)}
        onFocus={() => onJump?.(snippet)}
        onPaste={onPaste}
      />

      {/* Tags */}
      <div className="flex flex-wrap items-center gap-1 mt-1">
        {snippet.tags.map((t) => (
          <span key={t} className="inline-flex items-center gap-0.5 text-[10px] px-1.5 py-0.5 rounded bg-primary/10 text-primary">
            {t}
            <button onClick={() => onRemoveTag(snippet.id, t)} className="hover:text-red-400">
              <X size={10} />
            </button>
          </span>
        ))}
        <span className="inline-flex items-center gap-0.5 opacity-0 group-hover/card:opacity-100 transition-opacity">
          <input
            value={tagInput}
            onChange={(e) => setTagInput(e.target.value)}
            onKeyDown={(e) => { if (e.key === 'Enter') { e.preventDefault(); handleAddTag(); } }}
            className="w-16 text-[10px] bg-transparent text-text-secondary/40 placeholder:text-text-secondary/25 outline-none border-0"
            placeholder="标签…"
          />
          {tagInput && (
            <button onClick={handleAddTag} className="text-primary"><Tag size={10} /></button>
          )}
        </span>
      </div>

      {/* Translation display */}
      {(translation || isStreaming || streamError) && (
        <div className="mt-1.5 p-2 rounded bg-primary/5 border border-primary/10 text-text-primary leading-relaxed relative" style={{ fontSize }}>
          {streamError ? (
            <span className="block text-red-400 text-xs break-words" title={streamError}>
              翻译失败：{streamError}
            </span>
          ) : isStreaming && !streamText ? (
            <span className="inline-flex items-center gap-1.5 text-text-secondary/60 italic">
              <Loader2 size={11} className="animate-spin" />翻译中…
            </span>
          ) : (
            <>
              {streamText || translation}
              {isStreaming && (
                <span className="inline-block w-[2px] h-[1em] bg-primary/70 align-text-bottom animate-pulse ml-px" />
              )}
            </>
          )}
          {!isStreaming && !streamError && (
            <button
              onClick={handleCopyTranslation}
              className="absolute top-1 right-1 p-0.5 rounded hover:bg-surface-hover text-text-secondary/30 hover:text-text-secondary transition-colors"
              title="复制译文"
            >
              {transCopied ? <Check size={11} className="text-accent" /> : <Copy size={11} />}
            </button>
          )}
        </div>
      )}

      {/* Actions bar + timestamp */}
      <div className="flex items-center justify-between mt-1">
        <span className="text-[10px] text-text-secondary/30 flex items-center gap-0.5 opacity-0 group-hover/card:opacity-100 transition-opacity">
          <Clock size={10} />{timeStr}
        </span>
        <div className="flex items-center justify-end gap-0.5 opacity-0 group-hover/card:opacity-100 transition-opacity">
          <button
            onClick={handleCopy}
            className="shrink-0 p-0.5 rounded hover:bg-surface-hover text-text-secondary/30 hover:text-text-secondary transition-colors"
            title="复制原文"
          >
            {copied ? <Check size={11} className="text-accent" /> : <Copy size={11} />}
          </button>
          <button
            onClick={handleTranslate}
            className={`shrink-0 p-0.5 rounded transition-colors ${
              translation ? 'text-primary bg-primary/10' : 'text-text-secondary/30 hover:text-primary hover:bg-primary/5'
            }`}
            title={translation ? '隐藏译文' : '翻译'}
          >
            {isStreaming ? <Loader2 size={11} className="animate-spin" /> : <Languages size={11} />}
          </button>
          <button
            onClick={handleConvertToNote}
            className="shrink-0 p-0.5 rounded text-text-secondary/30 hover:text-primary hover:bg-primary/5 transition-colors"
            title="转为笔记"
          >
            <StickyNote size={11} />
          </button>
          <ConfirmButton icon onConfirm={() => onRemove(snippet.id)} confirmText="确认删除" aria-label="删除摘录">
            <Trash2 size={11} />
          </ConfirmButton>
        </div>
      </div>
    </div>
  );
}
