import { useCallback, useEffect, useRef, useState } from 'react';
import { listen } from '@tauri-apps/api/event';
import { X, Plus, Trash2, Play, Square, Download, Loader2, AlertTriangle, ChevronDown, ChevronRight } from 'lucide-react';
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
  saveTextFile,
  type AiTableColumn,
  type AiTableCell,
  type AiTableProgress,
  type AiTablePreset,
  type AiTablePaperBrief,
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
  const { open, paperIds, close } = useAiTableStore();
  const { alert, confirm, prompt } = useDialog();

  const [papers, setPapers] = useState<AiTablePaperBrief[]>([]);
  const [columns, setColumns] = useState<AiTableColumn[]>([
    { key: '研究方法', prompt: '该文献采用的研究方法/技术路线，一句话概括' },
  ]);
  const [presets, setPresets] = useState<AiTablePreset[]>([]);
  const [selectedPreset, setSelectedPreset] = useState('');
  const [cells, setCells] = useState<Map<string, AiTableCell>>(new Map());
  const [running, setRunning] = useState(false);
  const [progress, setProgress] = useState<AiTableProgress | null>(null);
  const [editing, setEditing] = useState<{ paperId: string; columnKey: string; value: string } | null>(null);
  const [providerId, setProviderId] = useState('');
  const [logs, setLogs] = useState<string[]>([]);
  const [showLog, setShowLog] = useState(true);
  const logRef = useRef<HTMLDivElement>(null);

  // Keep the process log pinned to its newest line.
  useEffect(() => {
    logRef.current?.scrollTo({ top: logRef.current.scrollHeight });
  }, [logs]);

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

  // Initial load: paper briefs + presets + any cached cells.
  useEffect(() => {
    if (!open) return;
    setPapers([]);
    setProgress(null);
    setRunning(false);
    aiTablePapersInfo(paperIds).then(setPapers).catch((err) => console.error('papers info:', err));
    aiTablePresetsList().then(setPresets).catch((err) => console.error('presets:', err));
  }, [open, paperIds]);

  useEffect(() => {
    if (!open) return;
    void reloadCells(columns);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open, columns.map((c) => c.key).join('|'), reloadCells]);

  // Progress events drive the run state; a finished run reloads the grid.
  useEffect(() => {
    if (!open) return;
    const un = listen<AiTableProgress>('library:ai-table-progress', (e) => {
      setProgress(e.payload);
      if (e.payload.log) {
        setLogs((prev) => [...prev.slice(-99), e.payload.log!]);
      }
      if (e.payload.finished) {
        setRunning(false);
        void reloadCells(columns);
      }
    });
    return () => {
      un.then((fn) => fn());
    };
  }, [open, columns, reloadCells]);

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

  const startRun = async (ids?: string[]) => {
    const cols = normalizedColumns();
    if (cols.length === 0) {
      await alert('请先添加至少一列');
      return;
    }
    setColumns(cols);
    setRunning(true);
    setProgress(null);
    setLogs([]);
    try {
      await aiTableExtractStart(ids ?? paperIds, cols, providerId || null);
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
        next.set(cellKey(paperId, columnKey), { paperId, columnKey, value, status: 'edited', edited: true });
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
  const doneCells = [...cells.values()].filter((c) => c.status === 'done' || c.edited).length;
  const totalCells = papers.length * colKeys.filter(Boolean).length;

  return (
    <div className="fixed inset-0 z-[6000] flex items-center justify-center">
      <div className="absolute inset-0 bg-black/50 backdrop-blur-sm" onClick={running ? undefined : close} />
      <div className="relative w-[94vw] max-w-[1200px] h-[84vh] mx-4 flex bg-surface border border-surface-hover rounded-xl shadow-2xl overflow-hidden">
        {/* Left: column definitions + presets */}
        <aside className="w-[260px] shrink-0 border-r border-surface-hover flex flex-col">
          <div className="px-3 py-2.5 border-b border-surface-hover">
            <span className="text-[13px] font-medium text-text-primary">列定义</span>
            <p className="text-[11px] text-text-secondary/60 mt-0.5">每列一个提取说明，对每篇文献抽取一次</p>
          </div>
          <div className="flex-1 overflow-y-auto p-2.5 space-y-2.5">
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
                  rows={2}
                  className="w-full bg-background border border-surface-hover rounded px-2 py-1 text-[11px] text-text-primary outline-none focus:border-primary/50 resize-none"
                />
              </div>
            ))}
            <button
              onClick={() => setColumns((prev) => [...prev, { key: '', prompt: '' }])}
              className="w-full flex items-center justify-center gap-1 px-2 py-1.5 rounded-lg border border-dashed border-surface-hover text-[12px] text-text-secondary hover:bg-surface-hover"
            >
              <Plus size={12} />
              添加列
            </button>
          </div>
          <div className="p-2.5 border-t border-surface-hover space-y-1.5">
            <select
              value={selectedPreset}
              onChange={(e) => applyPreset(e.target.value)}
              className="w-full bg-background border border-surface-hover rounded px-2 py-1.5 text-[12px] text-text-primary outline-none"
            >
              <option value="">选择模板…</option>
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
          <div className="p-2.5 border-t border-surface-hover space-y-1.5">
            <label className="block text-[11px] text-text-secondary/60">模型</label>
            <ModelOverrideSelect value={providerId} onChange={setProviderId} disabled={running} className="w-full" />
          </div>
        </aside>

        {/* Right: toolbar + grid */}
        <div className="flex-1 min-w-0 flex flex-col">
          <div className="flex items-center gap-2 px-4 py-2.5 border-b border-surface-hover">
            <span className="text-sm font-medium text-text-primary">AI 表格列</span>
            <span className="text-[11px] text-text-secondary/60">{papers.length} 篇文献</span>
            <div className="ml-auto flex items-center gap-2">
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
                <button
                  onClick={() => void startRun()}
                  className="flex items-center gap-1 px-2.5 py-1.5 rounded-lg bg-primary text-black text-[12px] font-medium hover:bg-primary/90"
                >
                  <Play size={12} />
                  {doneCells > 0 && doneCells < totalCells ? '继续抽取' : doneCells >= totalCells && totalCells > 0 ? '重新抽取缺失' : '开始抽取'}
                </button>
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
                onClick={close}
                disabled={running}
                className="p-1.5 rounded text-text-secondary/60 hover:text-text-primary hover:bg-surface-hover disabled:opacity-40"
              >
                <X size={15} />
              </button>
            </div>
          </div>

          <p className="px-4 py-1.5 text-[11px] text-text-secondary/50 border-b border-surface-hover/60">
            每篇文献一次调用返回所有列；已完成和手动编辑的单元格不会被覆盖。点击单元格可编辑。
          </p>

          {/* Process log: per-paper outcomes, so a long run never looks stuck */}
          <div className="border-b border-surface-hover/60">
            <button
              onClick={() => setShowLog((v) => !v)}
              className="w-full flex items-center gap-1 px-4 py-1 text-[11px] text-text-secondary/60 hover:bg-surface-hover/40"
            >
              {showLog ? <ChevronDown size={11} /> : <ChevronRight size={11} />}
              过程日志{logs.length > 0 ? `（${logs.length}）` : ''}
              {running && <Loader2 size={10} className="animate-spin" />}
            </button>
            {showLog && logs.length > 0 && (
              <div ref={logRef} className="max-h-28 overflow-y-auto px-4 pb-2 font-mono text-[11px] leading-relaxed text-text-secondary/80">
                {logs.map((l, i) => (
                  <div key={i}>{l}</div>
                ))}
              </div>
            )}
          </div>

          <div className="flex-1 overflow-auto">
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
                {papers.map((p) => (
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
                          className="px-3 py-2 align-top cursor-text"
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
                            <span className={cell.edited ? 'text-primary' : 'text-text-primary/90'}>
                              {cell.value}
                            </span>
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
              </tbody>
            </table>
          </div>
        </div>
      </div>
    </div>
  );
}
