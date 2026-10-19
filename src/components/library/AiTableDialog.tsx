import { useCallback, useEffect, useRef, useState } from 'react';
import { listen } from '@tauri-apps/api/event';
import { X, Plus, Trash2, Play, Square, Download, Loader2, AlertTriangle, PanelRight, PanelLeftClose, PanelLeftOpen, Columns3, SlidersHorizontal, RefreshCw, Search } from 'lucide-react';
import { useAiTableStore } from '@/stores/aiTableStore';
import { useDialog } from '@/hooks/useDialog';
import { ModelOverrideSelect } from './ModelOverrideSelect';
import {
  aiTableExtractStart,
  aiTableExtractCancel,
  aiTableCellsList,
  aiTableCellSet,
  aiTablePresetsList,
  aiTablePresetSave,
  aiTablePresetDelete,
  aiTablePapersInfo,
  aiTableExtractStatus,
  aiTableBuiltinPresets,
  aiTableTermsGet,
  aiTableTermsSet,
  aiTableTermsRegenerate,
  aiTableCellEvidence,
  saveTextFile,
  type AiTableColumn,
  type AiTableCell,
  type AiTableColumnTerms,
  type AiTableEvidenceRef,
  type AiTableProgress,
  type AiTablePreset,
  type AiTablePaperBrief,
  type AiTableBuiltinPreset,
  type AiTableDetail,
  type AiTableDepth,
  type AiTableVerify,
} from '@/lib/tauri';

const cellKey = (paperId: string, columnKey: string) => `${paperId}::${columnKey}`;

function fileStamp(): string {
  const d = new Date();
  const pad = (n: number) => String(n).padStart(2, '0');
  return `${d.getFullYear()}${pad(d.getMonth() + 1)}${pad(d.getDate())}-${pad(d.getHours())}${pad(d.getMinutes())}`;
}

/** Batch field extraction over a paper set: user-defined columns, one LLM
 *  call per paper, cached cells, editable grid, CSV/Markdown export. */
export function AiTableDialog() {
  const { open, paperIds, close, sidebarCollapsed, toggleSidebar, autoCollapseSidebar } = useAiTableStore();
  const { alert, confirm, prompt } = useDialog();

  const [papers, setPapers] = useState<AiTablePaperBrief[]>([]);
  const [columns, setColumns] = useState<AiTableColumn[]>([
    { key: '研究方法', prompt: '该文献采用的研究方法/技术路线，一句话概括' },
  ]);
  const [presets, setPresets] = useState<AiTablePreset[]>([]);
  const [selectedPreset, setSelectedPreset] = useState('');
  const [builtins, setBuiltins] = useState<AiTableBuiltinPreset[]>([]);
  /** How verbose the cells are extracted; the column prompt can override it. */
  const [detail, setDetail] = useState<AiTableDetail>('normal');
  /** How hard the run looks for evidence — cost and accuracy are the user's
   *  call, never silently traded by us. */
  const [depth, setDepth] = useState<AiTableDepth>('standard');
  /** Cross-check pass: only flagged cells (default), everything, or nothing. */
  const [verify, setVerify] = useState<AiTableVerify>('doubtful');
  const [cells, setCells] = useState<Map<string, AiTableCell>>(new Map());
  /** Stored search terms per column key, so a miss can be fixed by hand. */
  const [terms, setTerms] = useState<Record<string, AiTableColumnTerms>>({});
  /** Column key → the text currently in its term editor. */
  const [termDraft, setTermDraft] = useState<Record<string, string>>({});
  const [termsOpen, setTermsOpen] = useState<Record<string, boolean>>({});
  const [termBusy, setTermBusy] = useState<Record<string, boolean>>({});
  /** Only rows that have at least one doubtful cell. */
  const [onlySuspect, setOnlySuspect] = useState(false);
  /** Evidence panel: which cell is open, and its passages. */
  const [detailCell, setDetailCell] = useState<{ paperId: string; columnKey: string; title: string } | null>(null);
  const [detailRefs, setDetailRefs] = useState<AiTableEvidenceRef[] | null>(null);
  const [running, setRunning] = useState(false);
  const [progress, setProgress] = useState<AiTableProgress | null>(null);
  const [editing, setEditing] = useState<{ paperId: string; columnKey: string; value: string } | null>(null);
  const [providerId, setProviderId] = useState('');
  const [logs, setLogs] = useState<string[]>([]);
  /** Live reasoning of the paper being extracted (thinking models spend most of
   *  the call here; without it the panel looks frozen). */
  const [reasoning, setReasoning] = useState('');
  const [reasoningTitle, setReasoningTitle] = useState('');
  const [showLog, setShowLog] = useState(false);
  const [setupTab, setSetupTab] = useState<'columns' | 'settings'>('columns');
  const logRef = useRef<HTMLDivElement>(null);
  /** Throttled grid refresh while a run is in progress: cells are written per
   *  paper on the backend, so waiting for `finished` showed "等待" for minutes
   *  even though earlier papers were already done. */
  const reloadTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const reasoningRef = useRef<HTMLDivElement>(null);

  // Keep the process log pinned to its newest line.
  useEffect(() => {
    logRef.current?.scrollTo({ top: logRef.current.scrollHeight });
  }, [logs, showLog]);
  useEffect(() => {
    reasoningRef.current?.scrollTo({ top: reasoningRef.current.scrollHeight });
  }, [reasoning, showLog]);

  const reloadCells = useCallback(
    async (cols: AiTableColumn[]) => {
      const keys = cols.map((c) => c.key.trim()).filter(Boolean);
      if (paperIds.length === 0 || keys.length === 0) {
        setCells(new Map());
        return;
      }
      try {
        const list = await aiTableCellsList(paperIds, keys);
        setCells(new Map(list.map((c) => [cellKey(c.paperId, c.columnKey), c])));
      } catch (err) {
        console.error('load ai-table cells:', err);
      }
    },
    [paperIds]
  );

  /** Search terms are stored per column key: read them for whatever the editor
   *  currently holds (cache only, no model call). */
  const reloadTerms = useCallback(async (cols: AiTableColumn[]) => {
    const keys = cols.map((c) => c.key.trim()).filter(Boolean);
    if (keys.length === 0) {
      setTerms({});
      return;
    }
    try {
      const list = await aiTableTermsGet(keys);
      setTerms(Object.fromEntries(list.map((t) => [t.columnKey, t])));
    } catch (err) {
      console.error('load ai-table terms:', err);
    }
  }, []);

  const openEvidence = useCallback(async (paperId: string, columnKey: string, title: string) => {
    setDetailCell({ paperId, columnKey, title });
    setDetailRefs(null);
    try {
      setDetailRefs(await aiTableCellEvidence(paperId, columnKey));
    } catch (err) {
      console.error('load cell evidence:', err);
      setDetailRefs([]);
    }
  }, []);

  const saveTerms = async (key: string, raw: string) => {
    const list = raw
      .split(/[,，;；\s]+/)
      .map((t) => t.trim())
      .filter(Boolean);
    setTermBusy((prev) => ({ ...prev, [key]: true }));
    try {
      const list2 = await aiTableTermsSet(key, list);
      setTerms((prev) => ({ ...prev, [key]: list2[0] }));
      setTermDraft((prev) => ({ ...prev, [key]: list2[0]?.terms.join(' ') ?? '' }));
    } catch (err) {
      await alert(`保存检索词失败: ${err}`);
    } finally {
      setTermBusy((prev) => ({ ...prev, [key]: false }));
    }
  };

  const regenerateTerms = async (key: string, prompt: string) => {
    setTermBusy((prev) => ({ ...prev, [key]: true }));
    try {
      const list = await aiTableTermsRegenerate(key, prompt);
      setTermDraft((prev) => ({ ...prev, [key]: list.join(' ') }));
      await reloadTerms([{ key, prompt }]);
    } catch (err) {
      await alert(`生成检索词失败: ${err}`);
    } finally {
      setTermBusy((prev) => ({ ...prev, [key]: false }));
    }
  };

  // Initial load: paper briefs + presets + any cached cells.
  useEffect(() => {
    if (!open) return;
    setPapers([]);
    setProgress(null);
    setRunning(false);
    aiTablePapersInfo(paperIds).then(setPapers).catch((err) => console.error('papers info:', err));
    aiTablePresetsList().then(setPresets).catch((err) => console.error('presets:', err));
    aiTableBuiltinPresets().then(setBuiltins).catch((err) => console.error('builtin presets:', err));
    // A run may outlive this dialog (closing must not stop it): re-attach so the
    // stop button, the progress line and the log have something real behind them.
    aiTableExtractStatus()
      .then((s) => {
        if (!s.running) return;
        setRunning(true);
        const p = s.progress;
        // Belt-and-braces: a reasoning snapshot carries placeholder 0/0
        // counters and must never become the progress line.
        if (p && !p.reasoning) {
          setProgress(p);
          setLogs((prev) =>
            prev.length > 0 ? prev : [`↻ 接续进行中的抽取任务（${p.done}/${p.total}）`]
          );
        }
      })
      .catch((err) => console.error('extract status:', err));
  }, [open, paperIds]);

  useEffect(() => {
    if (!open) return;
    void reloadCells(columns);
    void reloadTerms(columns);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open, columns.map((c) => c.key).join('|'), reloadCells, reloadTerms]);

  // Progress events drive the run state; a finished run reloads the grid.
  useEffect(() => {
    if (!open) return;
    const un = listen<AiTableProgress>('library:ai-table-progress', (e) => {
      const p = e.payload;
      // A reasoning beat carries placeholder counters — keep the real progress.
      if (!p.reasoning) setProgress(p);
      if (p.reasoning) {
        setReasoning((prev) => prev + p.reasoning);
        if (p.currentTitle) setReasoningTitle(p.currentTitle);
        return;
      }
      // Per-paper start beat (no log): this paper's reasoning begins fresh.
      if (!p.log && p.currentTitle) {
        setReasoning('');
        setReasoningTitle(p.currentTitle);
      }
      if (p.log) {
        setLogs((prev) => [...prev.slice(-99), p.log!]);
        // A paper just finished: surface its cells now, not at the end.
        if (reloadTimer.current === null) {
          reloadTimer.current = setTimeout(() => {
            reloadTimer.current = null;
            void reloadCells(columns);
          }, 700);
        }
      }
      if (p.finished) {
        setRunning(false);
        void reloadCells(columns);
        // The run may have generated search terms for columns that had none.
        void reloadTerms(columns);
      }
    });
    return () => {
      un.then((fn) => fn());
      if (reloadTimer.current !== null) {
        clearTimeout(reloadTimer.current);
        reloadTimer.current = null;
      }
    };
  }, [open, columns, reloadCells, reloadTerms]);

  /** Trim keys, drop empties, dedupe — the canonical column list for a run. */
  const normalizedColumns = (): AiTableColumn[] => {
    const seen = new Set<string>();
    const out: AiTableColumn[] = [];
    for (const c of columns) {
      const key = c.key.trim();
      if (!key || seen.has(key)) continue;
      seen.add(key);
      out.push({ key, prompt: c.prompt.trim() });
    }
    return out;
  };

  const startRun = async (ids?: string[], force = false) => {
    const cols = normalizedColumns();
    if (cols.length === 0) {
      await alert('请先添加至少一列');
      return;
    }
    setColumns(cols);
    setRunning(true);
    setProgress(null);
    setLogs([]);
    setReasoning('');
    setReasoningTitle('');
    try {
      await aiTableExtractStart(ids ?? paperIds, cols, providerId || null, detail, depth, verify, force);
      // 开抽后表格主体才是重点：收起配置侧栏（用户手动展开过则不打扰）。
      autoCollapseSidebar();
    } catch (err) {
      setRunning(false);
      await alert(`启动抽取失败: ${err}`);
    }
  };

  const commitEdit = async () => {
    if (!editing) return;
    const { paperId, columnKey, value } = editing;
    setEditing(null);
    try {
      await aiTableCellSet(paperId, columnKey, value);
      setCells((prev) => {
        const next = new Map(prev);
        next.set(cellKey(paperId, columnKey), {
          paperId,
          columnKey,
          value,
          status: 'edited',
          edited: true,
          src: '',
          conf: 1,
          suspect: false,
          suspectReason: '',
          verifyVerdict: '',
          verifyNote: '',
        });
        return next;
      });
    } catch (err) {
      console.error('set cell:', err);
    }
  };

  const savePreset = async () => {
    const cols = normalizedColumns();
    if (cols.length === 0) {
      await alert('请先添加至少一列');
      return;
    }
    const name = await prompt('模板名称：', { placeholder: '如：实验类文献对比' });
    if (!name?.trim()) return;
    try {
      await aiTablePresetSave(null, name.trim(), cols);
      setPresets(await aiTablePresetsList());
    } catch (err) {
      await alert(`保存模板失败: ${err}`);
    }
  };

  const applyPreset = (id: string) => {
    setSelectedPreset(id);
    const p = presets.find((x) => x.id === id);
    if (p) setColumns(p.columns.map((c) => ({ ...c })));
  };

  const deletePreset = async () => {
    if (!selectedPreset) return;
    const p = presets.find((x) => x.id === selectedPreset);
    if (!p || !(await confirm(`删除模板「${p.name}」？`))) return;
    try {
      await aiTablePresetDelete(selectedPreset);
      setPresets(await aiTablePresetsList());
      setSelectedPreset('');
    } catch (err) {
      await alert(`删除模板失败: ${err}`);
    }
  };

  const saveFile = async (defaultPath: string, content: string, ext: string, label: string) => {
    try {
      const { save } = await import('@tauri-apps/plugin-dialog');
      const target = await save({ defaultPath, filters: [{ name: label, extensions: [ext] }] });
      if (target) await saveTextFile(target, content);
    } catch (err) {
      console.error('export ai-table:', err);
    }
  };

  const exportCsv = async () => {
    const esc = (v: string | number) => `"${String(v).replace(/"/g, '""')}"`;
    const header = ['标题', '年份', ...columns.map((c) => c.key)];
    const rows = papers.map((p) => [
      p.title,
      p.year ?? '',
      ...columns.map((c) => cells.get(cellKey(p.id, c.key))?.value ?? ''),
    ]);
    const csv = [header, ...rows].map((r) => r.map(esc).join(',')).join('\r\n');
    // BOM so Excel opens CJK correctly.
    await saveFile(`siku-表格-${fileStamp()}.csv`, '﻿' + csv, 'csv', 'CSV');
  };

  const exportMarkdown = async () => {
    const header = `| 标题 | 年份 | ${columns.map((c) => c.key).join(' | ')} |`;
    const sep = `|---|---|${columns.map(() => '---').join('|')}|`;
    const rows = papers.map(
      (p) =>
        `| ${p.title.replace(/\|/g, '\\|')} | ${p.year ?? ''} | ${columns
          .map((c) => (cells.get(cellKey(p.id, c.key))?.value ?? '').replace(/\|/g, '\\|'))
          .join(' | ')} |`
    );
    await saveFile(`siku-表格-${fileStamp()}.md`, [header, sep, ...rows].join('\n'), 'md', 'Markdown');
  };

  if (!open) return null;

  const colKeys = columns.map((c) => c.key.trim());
  // A cell the model could not fill is stored as `empty` (or, from older runs,
  // `done` with no text): it is 待补, so it must not count as finished.
  const isFilled = (c?: AiTableCell) => Boolean(c && (c.edited || c.value.trim().length > 0));
  const doneCells = [...cells.values()].filter(isFilled).length;
  const emptyCells = [...cells.values()].filter(
    (c) => !isFilled(c) && (c.status === 'empty' || c.status === 'done')
  ).length;
  /** 模型确认文中没有——不是待补，重试也没用（除非强制重抽）。 */
  const absentCells = [...cells.values()].filter((c) => !isFilled(c) && c.status === 'absent').length;
  /** 有值但站不住：无证据 / 无出处 / 置信度低 / 复核未通过。 */
  const isSuspect = (c?: AiTableCell) => Boolean(c && isFilled(c) && c.suspect);
  const suspectCells = [...cells.values()].filter(isSuspect).length;
  const visiblePapers = onlySuspect
    ? papers.filter((p) => colKeys.some((k) => k && isSuspect(cells.get(cellKey(p.id, k)))))
    : papers;
  const totalCells = papers.length * colKeys.filter(Boolean).length;
  /** 还要抽的格子：未填、且不是「文中无此」（那是最终答案，只有「重抽全部」才重判）。 */
  const pendingCells = Math.max(0, totalCells - doneCells - absentCells);

  return (
    <div className="fixed inset-0 z-[6000] flex items-center justify-center">
      {/* Closing is always allowed: the run keeps going in the background and
          the dialog re-attaches on reopen (ai_table_extract_status). Being
          trapped in a modal because a model call is slow is unacceptable. */}
      <div className="absolute inset-0 bg-black/50 backdrop-blur-sm" onClick={close} />
      <div className="relative w-[94vw] max-w-[1200px] h-[calc(100dvh-3rem)] max-h-[1200px] min-h-0 mx-4 flex bg-surface border border-surface-hover rounded-xl shadow-2xl overflow-hidden">
        {/* Left: switch between column definitions and extraction settings.
            Collapses to an icon rail so the grid owns the width once the
            setup is done. */}
        <aside
          className={`shrink-0 border-r border-surface-hover flex flex-col min-h-0 ${
            sidebarCollapsed ? 'w-9 items-center py-1.5 gap-0.5' : 'w-[280px]'
          }`}
        >
          {sidebarCollapsed ? (
            <>
              <button
                onClick={toggleSidebar}
                data-testid="ai-table-sidebar-expand"
                title="展开列定义与提取设置"
                className="w-7 h-7 rounded flex items-center justify-center text-text-secondary hover:text-text-primary hover:bg-surface-hover/60 transition-colors"
              >
                <PanelLeftOpen size={15} />
              </button>
              <div className="w-5 h-px bg-surface-hover my-1" />
              {(['columns', 'settings'] as const).map((tab) => (
                <button
                  key={tab}
                  onClick={() => {
                    setSetupTab(tab);
                    toggleSidebar();
                  }}
                  title={tab === 'columns' ? '列定义' : '提取设置'}
                  aria-label={tab === 'columns' ? '展开列定义' : '展开提取设置'}
                  className={`w-7 h-7 rounded flex items-center justify-center transition-colors ${
                    setupTab === tab
                      ? 'bg-background text-primary shadow-sm'
                      : 'text-text-secondary/60 hover:bg-surface-hover/60 hover:text-text-primary'
                  }`}
                >
                  {tab === 'columns' ? <Columns3 size={14} /> : <SlidersHorizontal size={14} />}
                </button>
              ))}
            </>
          ) : (
          <div
            role="tablist"
            aria-label="列定义与提取设置"
            className="flex items-center gap-1 p-1.5 border-b border-surface-hover"
            onKeyDown={(e) => {
              if (e.key !== 'ArrowLeft' && e.key !== 'ArrowRight') return;
              e.preventDefault();
              const next = setupTab === 'columns' ? 'settings' : 'columns';
              setSetupTab(next);
              document.getElementById(`ai-table-tab-${next}`)?.focus();
            }}
          >
            <div className="grid grid-cols-2 gap-1 flex-1">
            {(['columns', 'settings'] as const).map((tab) => (
              <button
                key={tab}
                id={`ai-table-tab-${tab}`}
                type="button"
                role="tab"
                aria-selected={setupTab === tab}
                aria-controls={`ai-table-panel-${tab}`}
                tabIndex={setupTab === tab ? 0 : -1}
                onClick={() => setSetupTab(tab)}
                className={`rounded-md px-3 py-2 text-[12px] font-medium transition-colors ${
                  setupTab === tab
                    ? 'bg-background text-text-primary shadow-sm'
                    : 'text-text-secondary/60 hover:bg-surface-hover/60 hover:text-text-primary'
                }`}
              >
                {tab === 'columns' ? '列定义' : '提取设置'}
              </button>
            ))}
            </div>
            <button
              onClick={toggleSidebar}
              data-testid="ai-table-sidebar-collapse"
              title="收起侧栏，表格区域更宽"
              className="w-7 h-7 shrink-0 rounded flex items-center justify-center text-text-secondary/60 hover:text-text-primary hover:bg-surface-hover/60 transition-colors"
            >
              <PanelLeftClose size={14} />
            </button>
          </div>
          )}
          {!sidebarCollapsed && (setupTab === 'columns' ? (
            <div
              id="ai-table-panel-columns"
              role="tabpanel"
              aria-labelledby="ai-table-tab-columns"
              className="flex-1 min-h-0 flex flex-col"
            >
              <section className="flex-1 min-h-0 overflow-y-auto p-2.5 space-y-2.5">
            {columns.map((col, i) => (
              <div key={i} className="rounded-lg border border-surface-hover/70 p-2 space-y-1.5">
                <div className="flex items-center gap-1.5">
                  <input
                    value={col.key}
                    onChange={(e) =>
                      setColumns((prev) => prev.map((c, j) => (j === i ? { ...c, key: e.target.value } : c)))
                    }
                    placeholder="列名（如：样本量）"
                    className="flex-1 min-w-0 bg-background border border-surface-hover rounded px-2 py-1 text-[12px] text-text-primary outline-none focus:border-primary/50"
                  />
                  <button
                    onClick={() => setColumns((prev) => prev.filter((_, j) => j !== i))}
                    className="p-1 rounded text-text-secondary/60 hover:text-red-400 hover:bg-surface-hover"
                    title="删除该列"
                  >
                    <Trash2 size={12} />
                  </button>
                </div>
                <textarea
                  value={col.prompt}
                  onChange={(e) =>
                    setColumns((prev) => prev.map((c, j) => (j === i ? { ...c, prompt: e.target.value } : c)))
                  }
                  placeholder="提取说明（留空则按列名提取）"
                  rows={3}
                  className="w-full min-h-[64px] max-h-40 resize-y overflow-y-auto bg-background border border-surface-hover rounded px-2 py-1 text-[11px] text-text-primary outline-none focus:border-primary/50"
                />
                {/* 检索词：这一列到底拿什么词去全文里找证据。可查看、可改、可重算 ——
                    "没抽到"十有八九是检索词没命中，用户必须能干预。 */}
                {(() => {
                  const key = col.key.trim();
                  if (!key) return null;
                  const info = terms[key];
                  const isOpen = Boolean(termsOpen[key]);
                  const busy = Boolean(termBusy[key]);
                  const label = !info || info.terms.length === 0 ? '未生成' : `${info.terms.length} 个`;
                  return (
                    <div className="pt-0.5">
                      <div className="flex items-center gap-1.5">
                        <button
                          onClick={() => {
                            setTermsOpen((prev) => ({ ...prev, [key]: !isOpen }));
                            setTermDraft((prev) => ({
                              ...prev,
                              [key]: prev[key] ?? (info?.terms.join(' ') ?? ''),
                            }));
                          }}
                          className="flex items-center gap-1 text-[10px] text-text-secondary/60 hover:text-text-primary"
                          title="该列在全文里检索用的词（决定证据能不能被找到）"
                        >
                          <Search size={10} />
                          检索词 · {label}
                          {info?.source === 'manual' && <span className="text-primary/80">自定义</span>}
                        </button>
                        {busy && <Loader2 size={10} className="animate-spin text-text-secondary/60" />}
                      </div>
                      {isOpen && (
                        <div className="mt-1 space-y-1 rounded border border-surface-hover/60 p-1.5">
                          <div className="flex flex-wrap gap-1">
                            {(info?.terms ?? []).length === 0 ? (
                              <span className="text-[10px] text-text-secondary/45">
                                首次抽取时自动生成（内置映射或一次 AI 生成）
                              </span>
                            ) : (
                              info!.terms.map((t) => (
                                <span
                                  key={t}
                                  className="px-1.5 py-0.5 rounded bg-surface-hover/50 text-[10px] text-text-secondary/80"
                                >
                                  {t}
                                </span>
                              ))
                            )}
                          </div>
                          <input
                            value={termDraft[key] ?? ''}
                            onChange={(e) => setTermDraft((prev) => ({ ...prev, [key]: e.target.value }))}
                            onKeyDown={(e) => {
                              if (e.key === 'Enter') {
                                e.preventDefault();
                                void saveTerms(key, termDraft[key] ?? '');
                              }
                              if (e.key === 'Escape') setTermsOpen((prev) => ({ ...prev, [key]: false }));
                            }}
                            placeholder="空格或逗号分隔，回车保存；清空回车=恢复默认"
                            className="w-full bg-background border border-surface-hover rounded px-1.5 py-1 text-[10px] text-text-primary outline-none focus:border-primary/50"
                          />
                          <div className="flex gap-1">
                            <button
                              onClick={() => void saveTerms(key, termDraft[key] ?? '')}
                              disabled={busy}
                              className="flex-1 px-1.5 py-1 rounded border border-surface-hover text-[10px] text-text-secondary hover:bg-surface-hover disabled:opacity-40"
                            >
                              保存
                            </button>
                            <button
                              onClick={() => void regenerateTerms(key, col.prompt)}
                              disabled={busy}
                              className="flex-1 px-1.5 py-1 rounded border border-surface-hover text-[10px] text-text-secondary hover:bg-surface-hover disabled:opacity-40"
                              title="丢掉当前词，重新按列名/说明生成（可能调用一次模型）"
                            >
                              AI 重算
                            </button>
                          </div>
                          <p className="text-[9px] text-text-secondary/40 leading-snug">
                            中文列名在英文论文里几乎检索不到命中，这些词就是用来跨语言对上证据的。
                          </p>
                        </div>
                      )}
                    </div>
                  );
                })()}
              </div>
            ))}
            <button
              onClick={() => setColumns((prev) => [...prev, { key: '', prompt: '' }])}
              className="w-full flex items-center justify-center gap-1 px-2 py-1.5 rounded-lg border border-dashed border-surface-hover text-[12px] text-text-secondary hover:bg-surface-hover"
            >
              <Plus size={12} />
              添加列
            </button>
              </section>
              <div className="shrink-0 p-2.5 border-t border-surface-hover space-y-1.5">
                <select
                  value=""
                  onChange={(e) => {
                    const b = builtins.find((x) => x.id === e.target.value);
                    if (b) setColumns(b.columns.map((c) => ({ ...c })));
                  }}
                  className="w-full bg-background border border-surface-hover rounded px-2 py-1.5 text-[12px] text-text-primary outline-none"
                  title={builtins.map((b) => `${b.name}：${b.description}`).join('\n')}
                >
                  <option value="">内置学科模板…</option>
                  {builtins.map((b) => (
                    <option key={b.id} value={b.id}>
                      {b.name}
                    </option>
                  ))}
                </select>
                <select
                  value={selectedPreset}
                  onChange={(e) => applyPreset(e.target.value)}
                  className="w-full bg-background border border-surface-hover rounded px-2 py-1.5 text-[12px] text-text-primary outline-none"
                >
                  <option value="">我的模板…</option>
                  {presets.map((p) => (
                    <option key={p.id} value={p.id}>
                      {p.name}
                    </option>
                  ))}
                </select>
                <div className="flex gap-1.5">
                  <button
                    onClick={() => void savePreset()}
                    className="flex-1 px-2 py-1.5 rounded-lg border border-surface-hover text-[12px] text-text-secondary hover:bg-surface-hover"
                  >
                    存为模板
                  </button>
                  <button
                    onClick={() => void deletePreset()}
                    disabled={!selectedPreset}
                    className="px-2 py-1.5 rounded-lg border border-surface-hover text-[12px] text-text-secondary hover:bg-surface-hover disabled:opacity-40"
                  >
                    删模板
                  </button>
                </div>
              </div>
            </div>
          ) : (
            <div
              id="ai-table-panel-settings"
              role="tabpanel"
              aria-labelledby="ai-table-tab-settings"
              className="flex-1 min-h-0 overflow-y-auto"
            >
              <div className="p-3 space-y-2">
                <p className="pb-1 text-[11px] leading-relaxed text-text-secondary/50">
                  设置本次抽取的深度、内容详略、交叉验证和模型。
                </p>
                <label className="block text-[11px] text-text-secondary/60">提取深度（成本 ↔ 精度）</label>
                <select
                  value={depth}
                  onChange={(e) => setDepth(e.target.value as AiTableDepth)}
                  disabled={running}
                  className="w-full bg-background border border-surface-hover rounded px-2 py-1.5 text-[12px] text-text-primary outline-none disabled:opacity-50"
                >
                  <option value="fast">快速 · 1 次/篇，3 段/列</option>
                  <option value="standard">标准 · 1 次 + 弱证据补判</option>
                  <option value="deep">深入 · 8 段/列，章节覆盖</option>
                  <option value="per_column">逐列精查 · 每列 1 次</option>
                </select>
                <p className="text-[10px] text-text-secondary/45 leading-snug">
                  {depth === 'fast'
                    ? '最省：证据 ≤6000 字，可能漏抽长文里的细节。'
                    : depth === 'standard'
                      ? '推荐：证据 ≤12000 字；证据不足的格子会再补判一次。'
                      : depth === 'deep'
                        ? '证据 ≤24000 字，按章节覆盖检索，适合精读与综述。'
                        : '每列单独检索并单独调用，最准也最贵（调用次数 ≈ 列数 × 篇数）。'}
                </p>
                <p className="text-[10px] text-text-secondary/45">
                  预计调用：{papers.length} 篇 ×{' '}
                  {depth === 'per_column'
                    ? `约 ${Math.max(1, columns.filter((c) => c.key.trim()).length)} 次`
                    : depth === 'standard'
                      ? '1~2 次'
                      : '1 次'}
                </p>
                <label className="block text-[11px] text-text-secondary/60 pt-2">详细程度</label>
                <select
                  value={detail}
                  onChange={(e) => setDetail(e.target.value as AiTableDetail)}
                  disabled={running}
                  className="w-full bg-background border border-surface-hover rounded px-2 py-1.5 text-[12px] text-text-primary outline-none disabled:opacity-50"
                >
                  <option value="brief">简洁（≤40 字）</option>
                  <option value="normal">标准（1~2 句）</option>
                  <option value="rich">详细（3~6 句，带数字）</option>
                </select>
                <p className="text-[10px] text-text-secondary/45 leading-snug">
                  列说明里写明长度/详略时以列说明为准；已抽取的单元格不会因改这里而重算。
                </p>
                <label className="block text-[11px] text-text-secondary/60 pt-2">交叉验证（额外调用）</label>
                <select
                  value={verify}
                  onChange={(e) => setVerify(e.target.value as AiTableVerify)}
                  disabled={running}
                  className="w-full bg-background border border-surface-hover rounded px-2 py-1.5 text-[12px] text-text-primary outline-none disabled:opacity-50"
                >
                  <option value="off">关闭 · 不额外调用</option>
                  <option value="doubtful">仅存疑格 · 每篇最多 6 格</option>
                  <option value="all">全部已抽取值 · 每篇最多 10 格</option>
                </select>
                <p className="text-[10px] text-text-secondary/45 leading-snug">
                  {verify === 'off'
                    ? '只做一次抽取；可疑的格子仍会被标出来，但不复核。'
                    : verify === 'doubtful'
                      ? '推荐：把无证据/无出处/置信度低的格子重新对照原文核对一次，核对通过即不再存疑。'
                      : '每篇多一次调用逐格核对，最严也最贵；被判定不支持的值会标为存疑。'}
                </p>
                <label className="block text-[11px] text-text-secondary/60 pt-2">模型</label>
                <ModelOverrideSelect value={providerId} onChange={setProviderId} disabled={running} className="w-full" />
              </div>
            </div>
          ))}
        </aside>

        {/* Right: toolbar + grid */}
        <div id="ai-table-grid-area" className="flex-1 min-w-0 min-h-0 flex flex-col">
          <div className="flex items-center gap-2 px-4 py-2.5 border-b border-surface-hover">
            <span className="text-sm font-medium text-text-primary">AI 表格列</span>
            <span className="text-[11px] text-text-secondary/60">{papers.length} 篇文献</span>
            <div className="ml-auto flex items-center gap-2">
              {suspectCells > 0 && (
                <button
                  onClick={() => setOnlySuspect((v) => !v)}
                  className={`flex items-center gap-1 px-2 py-1.5 rounded-lg border text-[12px] ${
                    onlySuspect
                      ? 'border-amber-400/60 text-amber-400 bg-amber-400/10'
                      : 'border-surface-hover text-text-secondary hover:bg-surface-hover'
                  }`}
                  title="只显示至少有 1 格存疑的文献"
                >
                  <AlertTriangle size={12} />
                  {onlySuspect ? '显示全部' : `只看存疑 ${suspectCells}`}
                </button>
              )}
              {progress && (
                <span className="text-[11px] text-text-secondary/70">
                  {progress.finished
                    ? progress.finished === 'cancelled'
                      ? `已取消（完成 ${progress.done}/${progress.total}）`
                      : `完成 ${progress.done}/${progress.total}${progress.failed > 0 ? `，失败 ${progress.failed}` : ''}`
                    : `抽取中 ${progress.done + progress.failed}/${progress.total}：${progress.currentTitle}`}
                </span>
              )}
              {running ? (
                <button
                  onClick={() => void aiTableExtractCancel()}
                  className="flex items-center gap-1 px-2.5 py-1.5 rounded-lg border border-surface-hover text-[12px] text-red-400 hover:bg-surface-hover"
                >
                  <Square size={12} />
                  停止
                </button>
              ) : (
                <>
                  <button
                    onClick={() => void startRun()}
                    disabled={totalCells > 0 && pendingCells === 0}
                    className="flex items-center gap-1 px-2.5 py-1.5 rounded-lg bg-primary text-black text-[12px] font-medium hover:bg-primary/90 disabled:opacity-40"
                  >
                    <Play size={12} />
                    {doneCells === 0
                      ? '开始抽取'
                      : pendingCells > 0
                        ? `补齐 ${pendingCells} 格`
                        : '全部已填写'}
                  </button>
                  {doneCells > 0 && (
                    <button
                      onClick={async () => {
                        if (
                          await confirm(
                            '重抽全部单元格？已抽取的内容会被覆盖，手工编辑过的单元格保持不变。'
                          )
                        ) {
                          void startRun(undefined, true);
                        }
                      }}
                      className="flex items-center gap-1 px-2 py-1.5 rounded-lg border border-surface-hover text-[12px] text-text-secondary hover:bg-surface-hover"
                      title="用当前的详细程度重新抽取，用于改完详略设置后刷新内容"
                    >
                      <RefreshCw size={12} />
                      重抽全部
                    </button>
                  )}
                </>
              )}
              <button
                onClick={() => void exportCsv()}
                disabled={papers.length === 0}
                className="flex items-center gap-1 px-2.5 py-1.5 rounded-lg border border-surface-hover text-[12px] text-text-secondary hover:bg-surface-hover disabled:opacity-40"
              >
                <Download size={12} />
                CSV
              </button>
              <button
                onClick={() => void exportMarkdown()}
                disabled={papers.length === 0}
                className="flex items-center gap-1 px-2.5 py-1.5 rounded-lg border border-surface-hover text-[12px] text-text-secondary hover:bg-surface-hover disabled:opacity-40"
              >
                <Download size={12} />
                Markdown
              </button>
              <button
                onClick={() => setShowLog((v) => !v)}
                title="模型推理 / 过程日志"
                aria-label="模型推理 / 过程日志"
                aria-expanded={showLog}
                aria-controls="ai-table-run-details"
                className={`p-1.5 rounded hover:bg-surface-hover ${showLog ? 'text-primary bg-primary/10' : 'text-text-secondary/60'}`}
              >
                <PanelRight size={14} />
              </button>
              <button
                onClick={close}
                title={running ? '关闭窗口不会中断抽取（可随时重新打开查看进度）' : '关闭'}
                className="p-1.5 rounded text-text-secondary/60 hover:text-text-primary hover:bg-surface-hover"
              >
                <X size={15} />
              </button>
            </div>
          </div>

          <p className="px-4 py-1.5 text-[11px] text-text-secondary/50 border-b border-surface-hover/60">
            每篇文献一次调用返回所有列；已完成和手动编辑的单元格不会被覆盖，抽取不到的字段记为「待补」可再次补齐。点击单元格可编辑，点「证据」看这一格用了哪些原文段落。{emptyCells > 0 ? ` 当前有 ${emptyCells} 格待补` : ''}{absentCells > 0 ? `、${absentCells} 格文中无此内容` : ''}{suspectCells > 0 ? `、${suspectCells} 格存疑（依据不足，建议核对）` : ''}。
          </p>

          <div className="flex-1 min-h-0 overflow-auto overscroll-contain">
            <table className="w-full border-collapse text-[12px]">
              <thead className="sticky top-0 bg-surface z-10">
                <tr>
                  <th className="text-left px-3 py-2 border-b border-surface-hover text-text-secondary font-medium min-w-[220px]">
                    标题
                  </th>
                  {colKeys.map(
                    (key, i) =>
                      key && (
                        <th
                          key={i}
                          className="text-left px-3 py-2 border-b border-surface-hover text-text-secondary font-medium min-w-[140px]"
                        >
                          {key}
                        </th>
                      )
                  )}
                </tr>
              </thead>
              <tbody>
                {visiblePapers.map((p) => (
                  <tr key={p.id} className="border-b border-surface-hover/50 hover:bg-surface-hover/30">
                    <td className="px-3 py-2 align-top">
                      <span className="block text-text-primary line-clamp-2">{p.title}</span>
                      {p.year && <span className="text-[10px] text-text-secondary/50">{p.year}</span>}
                    </td>
                    {colKeys.map((key, i) => {
                      if (!key) return null;
                      const cell = cells.get(cellKey(p.id, key));
                      const isEditing = editing?.paperId === p.id && editing?.columnKey === key;
                      return (
                        <td
                          key={i}
                          className="px-3 py-2 align-top cursor-text group"
                          onClick={() => {
                            if (!isEditing) setEditing({ paperId: p.id, columnKey: key, value: cell?.value ?? '' });
                          }}
                        >
                          {isEditing ? (
                            <textarea
                              autoFocus
                              value={editing.value}
                              onChange={(e) => setEditing({ ...editing, value: e.target.value })}
                              onBlur={() => void commitEdit()}
                              onKeyDown={(e) => {
                                if (e.key === 'Enter' && !e.shiftKey) {
                                  e.preventDefault();
                                  void commitEdit();
                                }
                                if (e.key === 'Escape') setEditing(null);
                              }}
                              rows={2}
                              className="w-full bg-background border border-primary/50 rounded px-1.5 py-1 text-[12px] text-text-primary outline-none resize-y"
                            />
                          ) : cell?.status === 'failed' ? (
                            <button
                              onClick={(e) => {
                                e.stopPropagation();
                                if (!running) void startRun([p.id]);
                              }}
                              className="flex items-center gap-1 text-amber-400 hover:text-amber-300"
                              title="抽取失败，点击重试该文献"
                            >
                              <AlertTriangle size={12} />
                              失败 · 重试
                            </button>
                          ) : running && !cell ? (
                            <span className="inline-flex items-center gap-1 text-text-secondary/50">
                              <Loader2 size={11} className="animate-spin" />
                              等待
                            </span>
                          ) : cell?.value ? (
                            <div className="space-y-1">
                              <span
                                className={`${cell.edited ? 'text-primary' : 'text-text-primary/90'} ${
                                  isSuspect(cell) ? 'underline decoration-amber-400/60 decoration-dotted' : ''
                                }`}
                                title={
                                  cell.edited
                                    ? '手动编辑'
                                    : [cell.src && `依据：${cell.src}`, cell.conf > 0 && `置信度 ${Math.round(cell.conf * 100)}%`]
                                        .filter(Boolean)
                                        .join(' · ') || undefined
                                }
                              >
                                {cell.value}
                              </span>
                              <div className="flex items-center gap-2 text-[10px]">
                                {isSuspect(cell) && (
                                  <button
                                    onClick={(e) => {
                                      e.stopPropagation();
                                      void openEvidence(p.id, key, p.title);
                                    }}
                                    className="flex items-center gap-0.5 text-amber-400/90 hover:text-amber-300"
                                    title={cell.suspectReason || '该值依据不足'}
                                  >
                                    <AlertTriangle size={10} />
                                    存疑
                                  </button>
                                )}
                                {cell.verifyVerdict === 'yes' && (
                                  <span className="text-emerald-500/70" title={cell.verifyNote || '已复核通过'}>
                                    已复核
                                  </span>
                                )}
                                {!cell.edited && (
                                  <button
                                    onClick={(e) => {
                                      e.stopPropagation();
                                      void openEvidence(p.id, key, p.title);
                                    }}
                                    className={`text-text-secondary/50 hover:text-primary ${
                                      isSuspect(cell) ? '' : 'opacity-0 group-hover:opacity-100'
                                    }`}
                                    title="查看这一格的原文依据"
                                  >
                                    证据
                                  </button>
                                )}
                              </div>
                            </div>
                          ) : cell?.status === 'absent' ? (
                            // 模型读了证据后确认文中没有：这是最终答案，不是待补
                            <button
                              onClick={(e) => {
                                e.stopPropagation();
                                if (!running) void startRun([p.id], true);
                              }}
                              className="text-text-secondary/45 hover:text-text-secondary/70"
                              title="模型确认本文没有该内容（点击可强制重抽这一篇）"
                            >
                              文中无此
                            </button>
                          ) : cell && (cell.status === 'empty' || cell.status === 'done') ? (
                            // Extracted but the model found nothing here: 待补
                            // (this used to be cached as "done" and could never
                            // be refilled).
                            <button
                              onClick={(e) => {
                                e.stopPropagation();
                                if (!running) void startRun([p.id]);
                              }}
                              className="flex items-center gap-1 text-amber-400/80 hover:text-amber-300"
                              title="该字段未抽取到内容——原因见右侧「模型推理 / 过程日志」；点击只重试这一篇"
                            >
                              待补 · 重试
                            </button>
                          ) : (
                            <span className="text-text-secondary/40">—</span>
                          )}
                        </td>
                      );
                    })}
                  </tr>
                ))}
                {papers.length === 0 && (
                  <tr>
                    <td colSpan={colKeys.filter(Boolean).length + 1} className="px-3 py-8 text-center text-text-secondary/50">
                      文献加载中…
                    </td>
                  </tr>
                )}
                {papers.length > 0 && visiblePapers.length === 0 && (
                  <tr>
                    <td colSpan={colKeys.filter(Boolean).length + 1} className="px-3 py-8 text-center text-text-secondary/50">
                      没有存疑的格子
                    </td>
                  </tr>
                )}
              </tbody>
            </table>
          </div>
        </div>

        {/* Right drawer: inline (flex) rather than overlay, so expanding it
            shrinks the grid and collapsing it returns the space to the grid. */}
        <aside
          id="ai-table-run-details"
          aria-label="模型推理与过程日志"
          aria-hidden={!showLog}
          className={`shrink-0 min-h-0 flex flex-col overflow-hidden transition-all duration-200 ${
            showLog ? 'w-[300px] border-l border-surface-hover opacity-100' : 'w-0 opacity-0 pointer-events-none'
          }`}
        >
          <div className="w-[300px] min-w-[300px] flex flex-col h-full">
            {/* 模型推理：thinking 模型的绝大部分时间花在这里，不显示就只是一片沉默 */}
            <div className="flex items-center gap-1.5 px-3 py-2 border-b border-surface-hover/60 text-[11px] text-text-secondary/70">
              <span className="shrink-0">模型推理</span>
              {reasoning && reasoningTitle && (
                <span className="truncate text-text-secondary/45">· 《{reasoningTitle}》</span>
              )}
              {running && reasoning && <Loader2 size={10} className="animate-spin shrink-0" />}
              <button
                onClick={() => setShowLog(false)}
                title="收起运行详情"
                tabIndex={showLog ? 0 : -1}
                className="ml-auto shrink-0 p-1 rounded text-text-secondary/60 hover:text-text-primary hover:bg-surface-hover"
              >
                <X size={13} />
              </button>
            </div>
            <div
              ref={reasoningRef}
              className={`overflow-y-auto px-3 py-2 whitespace-pre-wrap break-words font-mono text-[10px] leading-relaxed text-text-secondary/50 italic ${
                reasoning ? 'basis-[45%] max-h-[45%] border-b border-surface-hover/50' : 'hidden'
              }`}
            >
              {reasoning}
            </div>
            <div className="flex items-center gap-1.5 px-3 py-2 border-b border-surface-hover text-[12px] text-text-secondary">
              过程日志
              {running && <Loader2 size={11} className="animate-spin" />}
              {logs.length > 0 && <span className="text-text-secondary/40">（{logs.length}）</span>}
            </div>
            <div
              ref={logRef}
              className="flex-1 min-h-0 overflow-y-auto px-3 py-2 font-mono text-[11px] leading-relaxed text-text-secondary/80 space-y-1"
            >
              {logs.length === 0 ? (
                <span className="text-text-secondary/40">运行后此处逐篇显示抽取结果。</span>
              ) : (
                logs.map((l, i) => <div key={i}>{l}</div>)
              )}
            </div>
          </div>
        </aside>

        {/* 证据面板：这一格的原文依据。抽取值旁边没有证据可查，表格就只能是"看起来对" */}
        {detailCell && (
          <div
            className="absolute inset-0 z-20 flex items-center justify-center bg-black/40 p-6"
            onClick={() => setDetailCell(null)}
          >
            <div
              className="w-[620px] max-h-full flex flex-col bg-surface border border-surface-hover rounded-xl shadow-2xl overflow-hidden"
              onClick={(e) => e.stopPropagation()}
            >
              {(() => {
                const cell = cells.get(cellKey(detailCell.paperId, detailCell.columnKey));
                return (
                  <>
                    <div className="flex items-start gap-2 px-4 py-2.5 border-b border-surface-hover">
                      <div className="min-w-0">
                        <p className="text-[12px] text-text-primary truncate">《{detailCell.title}》</p>
                        <p className="text-[11px] text-text-secondary/60">
                          {detailCell.columnKey}
                          {cell?.src ? ` · 模型标注位置 ${cell.src}` : ' · 模型未标注位置'}
                          {cell && cell.conf > 0 ? ` · 置信度 ${Math.round(cell.conf * 100)}%` : ''}
                        </p>
                      </div>
                      <button
                        onClick={() => setDetailCell(null)}
                        className="ml-auto p-1 rounded text-text-secondary/60 hover:text-text-primary hover:bg-surface-hover"
                      >
                        <X size={14} />
                      </button>
                    </div>
                    <div className="flex-1 min-h-0 overflow-y-auto px-4 py-3 space-y-2.5">
                      {cell?.value && (
                        <div className="rounded-lg bg-surface-hover/40 px-2.5 py-2 text-[12px] text-text-primary">
                          {cell.value}
                        </div>
                      )}
                      {cell?.suspect && (
                        <div className="flex items-start gap-1.5 rounded-lg border border-amber-400/40 bg-amber-400/5 px-2.5 py-2 text-[11px] text-amber-400/90">
                          <AlertTriangle size={12} className="mt-0.5 shrink-0" />
                          <span>{cell.suspectReason || '依据不足'}</span>
                        </div>
                      )}
                      {cell?.verifyVerdict && cell.verifyVerdict !== 'yes' && (
                        <div className="rounded-lg border border-amber-400/40 px-2.5 py-2 text-[11px] text-amber-400/90">
                          复核结论：{cell.verifyVerdict === 'no' ? '证据不支持' : '证据仅部分支持'}
                          {cell.verifyNote ? `——${cell.verifyNote}` : ''}
                        </div>
                      )}
                      {cell?.verifyVerdict === 'yes' && (
                        <div className="rounded-lg border border-emerald-500/30 px-2.5 py-2 text-[11px] text-emerald-500/80">
                          复核通过{cell.verifyNote ? `：${cell.verifyNote}` : ''}
                        </div>
                      )}
                      {cell?.edited && (
                        <p className="text-[11px] text-text-secondary/60">
                          这一格是手动填写，没有 AI 证据链。
                        </p>
                      )}
                      {detailRefs === null ? (
                        <div className="flex items-center gap-1.5 text-[11px] text-text-secondary/60">
                          <Loader2 size={11} className="animate-spin" />
                          读取证据…
                        </div>
                      ) : detailRefs.length === 0 ? (
                        <p className="text-[11px] text-text-secondary/60">
                          没有记录到证据段落（可能是早于证据留存的旧数据，或该篇没有全文索引）。用「重抽全部」可补上。
                        </p>
                      ) : (
                        <div className="space-y-2">
                          <p className="text-[11px] text-text-secondary/60">
                            本次调用给模型的 {detailRefs.length} 段原文（标「本列」的是由这一列的检索词命中）：
                          </p>
                          {detailRefs
                            .slice()
                            .sort(
                              (a, b) =>
                                Number(b.fromColumn === detailCell.columnKey) -
                                Number(a.fromColumn === detailCell.columnKey)
                            )
                            .map((r, idx) => (
                              <div key={idx} className="rounded-lg border border-surface-hover/70 px-2.5 py-2 space-y-1">
                                <div className="flex items-center gap-2 text-[10px] text-text-secondary/60">
                                  <span className="font-mono text-primary/70">{r.label || '（无页码）'}</span>
                                  {r.fromColumn === detailCell.columnKey ? (
                                    <span className="px-1 rounded bg-primary/15 text-primary/80">本列</span>
                                  ) : (
                                    r.fromColumn && <span>· 由「{r.fromColumn}」检索命中</span>
                                  )}
                                </div>
                                <p className="text-[11px] text-text-primary/80 whitespace-pre-wrap break-words leading-relaxed">
                                  {r.snippet}
                                </p>
                              </div>
                            ))}
                        </div>
                      )}
                    </div>
                  </>
                );
              })()}
            </div>
          </div>
        )}
      </div>
    </div>
  );
}
