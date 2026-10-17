import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { listen } from '@tauri-apps/api/event';
import { useQueryClient } from '@tanstack/react-query';
import {
  X,
  Loader2,
  Sparkles,
  Play,
  Square,
  Plus,
  Trash2,
  Check,
  Undo2,
  Tags,
  FolderTree,
  Eye,
  ChevronDown,
  ChevronRight,
  RefreshCw,
  BookMarked,
  Download,
  Cpu,
  Activity,
} from 'lucide-react';
import { useAiClassifyStore } from '@/stores/aiClassifyStore';
import { useDialog } from '@/hooks/useDialog';
import { ModelOverrideSelect } from './ModelOverrideSelect';
import {
  AI_CLASSIFY_PROGRESS_EVENT,
  aiClassifyApply,
  aiClassifyCancel,
  aiClassifyGroupsSet,
  aiClassifyItemsSet,
  aiClassifyReassign,
  aiClassifyRunGet,
  aiClassifyStart,
  aiClassifyUndo,
  aiClassifyCapabilities,
  aiClassifyVocabList,
  aiClassifyVocabSetStatus,
  saveTextFile,
  type ApplySummary,
  type ClassifyGroup,
  type ClassifyItem,
  type ClassifyProgress,
  type ClassifyRun,
  type UndoSummary,
  type VocabEntry,
  type ClassifyCapabilities,
} from '@/lib/tauri';

type Step = 'config' | 'review' | 'result';
type Target = 'collection' | 'tag' | 'preview';
type ReviewTab = 'all' | 'pending' | 'accepted';

const DIMENSIONS = ['研究主题', '研究方法', '研究阶段', '数据类型'];
const SAMPLE_SIZES = [10, 20, 30];
const THRESHOLDS = [
  { value: 0.5, label: '少审（0.5）' },
  { value: 0.75, label: '标准（0.75）' },
  { value: 0.9, label: '只信高把握（0.9）' },
];
/** Papers per draft call / assign call — mirrors core::ai_classify. */
const DRAFT_BATCH = 24;
const ASSIGN_BATCH = 20;
const MAX_LABELS = 3;

function estimateCalls(papers: number, sample: boolean, sampleSize: number, twoLevel: boolean) {
  const drafted = sample ? Math.min(sampleSize, papers) : papers;
  const draftCalls = Math.ceil(drafted / DRAFT_BATCH) + 1 + (twoLevel ? 1 : 0);
  const fullAssignCalls = Math.ceil(papers / ASSIGN_BATCH);
  return { draftCalls, fullAssignCalls, drafted };
}

/** `父/子` → segments (paths never exceed two levels). */
function splitPath(path: string): string[] {
  return path
    .split('/')
    .map((s) => s.trim())
    .filter(Boolean);
}

function joinPath(segments: string[]): string {
  return segments
    .map((s) => s.trim())
    .filter(Boolean)
    .join('/');
}

/** Every path in the draft (parents included) — the closed set for selects. */
function treePaths(groups: ClassifyGroup[]): string[] {
  const out: string[] = [];
  for (const g of groups) {
    const name = g.name.trim();
    if (!name) continue;
    out.push(name);
    for (const c of g.children) {
      const child = c.name.trim();
      if (child) out.push(`${name}/${child}`);
    }
  }
  return out;
}

/** 已接受 / 待审 counts for one group path (a tag-mode paper counts once per
 *  label it carries). */
function countsFor(items: ClassifyItem[], path: string) {
  let accepted = 0;
  let pending = 0;
  for (const it of items) {
    if (!it.labels.includes(path)) continue;
    if (it.status === 'accepted' || it.edited) accepted += 1;
    else pending += 1;
  }
  return { accepted, pending };
}

/** AI 文献分类: draft a taxonomy → review/edit → write back to collections or
 *  tags, with a one-click undo of everything the run wrote. */
export function AiClassifyDialog() {
  const { open, paperIds, scope, close } = useAiClassifyStore();
  const { alert, confirm } = useDialog();
  const queryClient = useQueryClient();

  const [step, setStep] = useState<Step>('config');
  const [dimension, setDimension] = useState('研究主题');
  const [customDimension, setCustomDimension] = useState('');
  const [target, setTarget] = useState<Target>('collection');
  const [sampleFirst, setSampleFirst] = useState(true);
  const [sampleSize, setSampleSize] = useState(10);
  const [reuseExisting, setReuseExisting] = useState(true);
  const [maxGroups, setMaxGroups] = useState(12);
  const [twoLevel, setTwoLevel] = useState(true);
  const [threshold, setThreshold] = useState(0.75);
  const [skipExisting, setSkipExisting] = useState(false);
  const [engine, setEngine] = useState<'batch' | 'semantic'>('batch');
  const [capabilities, setCapabilities] = useState<ClassifyCapabilities | null>(null);
  const [providerId, setProviderId] = useState('');

  const [run, setRun] = useState<ClassifyRun | null>(null);
  const [groups, setGroups] = useState<ClassifyGroup[]>([]);
  const [items, setItems] = useState<ClassifyItem[]>([]);
  const [running, setRunning] = useState(false);
  const [progress, setProgress] = useState<ClassifyProgress | null>(null);
  const [logs, setLogs] = useState<string[]>([]);
  const [showLog, setShowLog] = useState(false);
  const [tab, setTab] = useState<ReviewTab>('all');
  const [applySummary, setApplySummary] = useState<ApplySummary | null>(null);
  const [undoSummary, setUndoSummary] = useState<UndoSummary | null>(null);
  const [busy, setBusy] = useState(false);
  const [vocabOpen, setVocabOpen] = useState(false);
  const [vocab, setVocab] = useState<VocabEntry[]>([]);
  const [collapsed, setCollapsed] = useState<Set<string>>(new Set());
  /** Vocabulary counts for the config-step summary card. */
  const [vocabStats, setVocabStats] = useState<{
    active: number;
    staged: number;
    rejected: number;
  } | null>(null);

  const logRef = useRef<HTMLDivElement>(null);
  // Async handlers (event listener, blur commits) need the latest values.
  const runIdRef = useRef<string | null>(null);
  const groupsRef = useRef<ClassifyGroup[]>([]);
  const itemsRef = useRef<ClassifyItem[]>([]);
  groupsRef.current = groups;
  itemsRef.current = items;

  useEffect(() => {
    logRef.current?.scrollTo({ top: logRef.current.scrollHeight });
  }, [logs]);

  const loadRun = useCallback(async (id: string) => {
    const r = await aiClassifyRunGet(id);
    setRun(r);
    setGroups(r.groups);
    setItems(r.items);
    setStep('review');
  }, []);

  const loadVocab = useCallback(async (dim: string, tgt: string) => {
    try {
      setVocab(await aiClassifyVocabList(dim, tgt));
    } catch (err) {
      console.error('load vocab:', err);
    }
  }, []);

  // Reset whenever the dialog is (re)opened with a paper set.
  useEffect(() => {
    if (!open) return;
    setStep('config');
    runIdRef.current = null;
    setRun(null);
    setGroups([]);
    setItems([]);
    setRunning(false);
    setProgress(null);
    setLogs([]);
    setTab('all');
    setApplySummary(null);
    setUndoSummary(null);
    setBusy(false);
    setVocabOpen(false);
    setCollapsed(new Set());
    aiClassifyCapabilities()
      .then(setCapabilities)
      .catch(() => setCapabilities({ semantic: false }));
  }, [open, paperIds]);

  // Progress events drive the run state; a finished draft loads the grid.
  useEffect(() => {
    if (!open) return;
    const un = listen<ClassifyProgress>(AI_CLASSIFY_PROGRESS_EVENT, (e) => {
      const p = e.payload;
      setProgress(p);
      if (p.log) setLogs((prev) => [...prev.slice(-99), p.log!]);
      if (p.finished) {
        setRunning(false);
        if (p.finished === 'done' && runIdRef.current) {
          void loadRun(runIdRef.current).catch((err) => console.error('load classify run:', err));
        }
      }
    });
    return () => {
      un.then((fn) => fn());
    };
  }, [open, loadRun]);

  const dimensionValue = dimension === '自定义' ? customDimension.trim() : dimension;
  const papers = paperIds.length;
  const est = estimateCalls(papers, sampleFirst, sampleSize, twoLevel);
  const multiLabel = target === 'tag';

  // Vocabulary summary for the run panel: what this dimension already knows.
  useEffect(() => {
    if (!open || step !== 'config') return;
    const dim = dimensionValue.trim();
    if (!dim) {
      setVocabStats(null);
      return;
    }
    let alive = true;
    aiClassifyVocabList(dim, target)
      .then((rows) => {
        if (!alive) return;
        setVocabStats({
          active: rows.filter((r) => r.status === 'active').length,
          staged: rows.filter((r) => r.status === 'staged').length,
          rejected: rows.filter((r) => r.status === 'rejected').length,
        });
      })
      .catch(() => {
        if (alive) setVocabStats(null);
      });
    return () => {
      alive = false;
    };
  }, [open, step, dimensionValue, target]);

  const paths = useMemo(() => treePaths(groups), [groups]);
  const accepted = useMemo(
    () => items.filter((i) => i.labels.length > 0 && (i.status === 'accepted' || i.edited)),
    [items]
  );
  const pending = useMemo(
    () => items.filter((i) => i.status === 'pending' && !i.edited),
    [items]
  );
  const unassigned = useMemo(() => items.filter((i) => i.labels.length === 0), [items]);
  /** Papers in the run that the draft never looked at (sample-first). */
  const remaining = run ? Math.max(0, run.paperCount - items.length) : 0;

  const start = async (dimOverride?: string) => {
    const dim = (dimOverride ?? dimensionValue).trim();
    if (papers === 0) {
      await alert('该范围内没有文献');
      return;
    }
    if (!dim) {
      await alert('请填写分类维度');
      return;
    }
    setRunning(true);
    setProgress(null);
    setLogs([]);
    setApplySummary(null);
    setUndoSummary(null);
    try {
      const id = await aiClassifyStart({
        paperIds,
        scopeKind: scope.kind,
        scopeId: scope.collectionId,
        dimension: dim,
        target,
        sampleSize: sampleFirst ? sampleSize : null,
        reuseExisting,
        maxGroups,
        twoLevel,
        threshold,
        skipExisting,
        engine,
        providerId: providerId || null,
      });
      runIdRef.current = id;
      void loadVocab(dim, target);
    } catch (err) {
      setRunning(false);
      await alert(`启动分类失败: ${err}`);
    }
  };

  /** Push the dialog's current assignments to the DB. `edited` is omitted on
   *  purpose: this is a sync, not a user edit, so rerun protection is kept. */
  const syncItems = async () => {
    const id = runIdRef.current;
    if (!id) return;
    const rows = itemsRef.current.map((it) => ({ paperId: it.paperId, labels: it.labels }));
    if (rows.length > 0) await aiClassifyItemsSet(id, rows);
  };

  const persistGroups = async (next?: ClassifyGroup[]) => {
    const id = runIdRef.current;
    if (!id) return;
    await aiClassifyGroupsSet(id, next ?? groupsRef.current);
  };

  // --- draft tree edits -----------------------------------------------------

  const renameLocal = (path: string, nextName: string) => {
    const segs = splitPath(path);
    setGroups((prev) =>
      prev.map((g) => {
        if (segs.length === 1) {
          return g.name === segs[0] ? { ...g, name: nextName } : g;
        }
        if (g.name !== segs[0]) return g;
        return {
          ...g,
          children: g.children.map((c) => (c.name === segs[1] ? { ...c, name: nextName } : c)),
        };
      })
    );
  };

  /** Committing a rename also carries every assignment that referenced the old
   *  path (`父/子` moves with its parent). */
  const commitRename = async (oldPath: string, nextName: string) => {
    const id = runIdRef.current;
    const trimmed = nextName.trim();
    const segs = splitPath(oldPath);
    const nextPath = joinPath([...segs.slice(0, -1), trimmed]);
    if (!trimmed || nextPath === oldPath) {
      if (id) await persistGroups().catch(() => undefined);
      return;
    }
    const remap = (label: string) => {
      if (label === oldPath) return nextPath;
      if (label.startsWith(`${oldPath}/`)) return `${nextPath}${label.slice(oldPath.length)}`;
      return label;
    };
    const affected = itemsRef.current
      .map((it) => ({ it, labels: it.labels.map(remap) }))
      .filter(({ it, labels }) => labels.join('\u0001') !== it.labels.join('\u0001'));
    setItems((prev) =>
      prev.map((it) => {
        const labels = it.labels.map(remap);
        return { ...it, labels, groupName: labels[0] ?? '' };
      })
    );
    if (!id) return;
    try {
      await persistGroups();
      if (affected.length > 0) {
        await aiClassifyItemsSet(
          id,
          affected.map(({ it, labels }) => ({ paperId: it.paperId, labels, edited: true }))
        );
      }
    } catch (err) {
      console.error('persist group rename:', err);
    }
  };

  const removeGroup = async (path: string) => {
    const segs = splitPath(path);
    const parent = groups.find((g) => g.name === segs[0]);
    const node = segs.length === 1 ? parent : parent?.children.find((c) => c.name === segs[1]);
    if (!node) return;
    const subtree = [path, ...node.children.map((c) => `${path}/${c.name.trim()}`)];
    const affected = itemsRef.current.filter((it) => it.labels.some((l) => subtree.includes(l)));
    if (
      !(await confirm(`删除类目「${path}」？其中 ${affected.length} 篇文献的相关归属会变为待定。`))
    )
      return;
    const next =
      segs.length === 1
        ? groups.filter((g) => g.name !== segs[0])
        : groups.map((g) =>
            g.name === segs[0]
              ? { ...g, children: g.children.filter((c) => c.name !== segs[1]) }
              : g
          );
    setGroups(next);
    setItems((prev) =>
      prev.map((it) => {
        const labels = it.labels.filter((l) => !subtree.includes(l));
        return labels.length === it.labels.length ? it : { ...it, labels, groupName: labels[0] ?? '' };
      })
    );
    const id = runIdRef.current;
    if (!id) return;
    try {
      await aiClassifyGroupsSet(id, next);
      const rows = itemsRef.current
        .map((it) => ({ it, labels: it.labels.filter((l) => !subtree.includes(l)) }))
        .filter(({ it, labels }) => labels.length !== it.labels.length);
      if (rows.length > 0) {
        await aiClassifyItemsSet(
          id,
          rows.map(({ it, labels }) => ({ paperId: it.paperId, labels, edited: true }))
        );
      }
    } catch (err) {
      console.error('remove group:', err);
    }
  };

  const addGroup = async (parentPath: string | null) => {
    const draft: ClassifyGroup = { name: '', description: '', existingId: null, children: [] };
    const parentName = parentPath ? splitPath(parentPath)[0] : null;
    const next = parentName
      ? groups.map((g) =>
          g.name === parentName ? { ...g, children: [...g.children, draft] } : g
        )
      : [...groups, draft];
    setGroups(next);
    try {
      await persistGroups(next);
    } catch (err) {
      console.error('add group:', err);
    }
  };

  // --- assignment edits -----------------------------------------------------

  const moveItem = async (paperId: string, labels: string[]) => {
    const cleaned = labels
      .map((l) => l.trim())
      .filter(Boolean)
      .slice(0, MAX_LABELS);
    setItems((prev) =>
      prev.map((it) =>
        it.paperId === paperId
          ? {
              ...it,
              labels: cleaned,
              groupName: cleaned[0] ?? '',
              edited: true,
              status: 'accepted',
            }
          : it
      )
    );
    const id = runIdRef.current;
    if (!id) return;
    try {
      await aiClassifyItemsSet(id, [{ paperId, labels: cleaned, edited: true }]);
    } catch (err) {
      console.error('move item:', err);
    }
  };

  /** Bulk triage of the review queue: accept the suggestion or clear it. */
  const resolvePending = async (accept: boolean) => {
    const id = runIdRef.current;
    if (!id) return;
    const targets = pending.filter((it) => it.labels.length > 0);
    if (targets.length === 0) {
      await alert('待审列表里没有带归属的文献');
      return;
    }
    setItems((prev) =>
      prev.map((it) =>
        targets.some((t) => t.paperId === it.paperId)
          ? {
              ...it,
              labels: accept ? it.labels : [],
              groupName: accept ? it.groupName : '',
              edited: true,
              status: 'accepted',
            }
          : it
      )
    );
    try {
      await aiClassifyItemsSet(
        id,
        targets.map((it) => ({
          paperId: it.paperId,
          labels: accept ? it.labels : [],
          edited: true,
        }))
      );
    } catch (err) {
      console.error('resolve pending:', err);
    }
  };

  const reassign = async (onlyMissing: boolean) => {
    const id = runIdRef.current;
    if (!id) return;
    if (paths.length === 0) {
      await alert('请先添加至少一个类目');
      return;
    }
    if (!onlyMissing) {
      const skip = items.filter((it) => it.edited).length;
      const ok = await confirm(
        `将用当前 ${paths.length} 个类目重新判定全部文献${
          skip > 0 ? `（其中 ${skip} 篇手动改过的会被保留）` : ''
        }。继续？`
      );
      if (!ok) return;
    }
    setRunning(true);
    setProgress(null);
    try {
      await persistGroups();
      await syncItems();
      await aiClassifyReassign(id, providerId || null, onlyMissing);
    } catch (err) {
      setRunning(false);
      await alert(`判定失败: ${err}`);
    }
  };

  const apply = async () => {
    const id = runIdRef.current;
    if (!id || !run) return;
    if (target === 'preview') return;
    if (paths.length === 0) {
      await alert('请先添加至少一个类目');
      return;
    }
    if (accepted.length === 0) {
      await alert('还没有任何已接受的归属（待审的不会写入，请先在「待审」里确认）');
      return;
    }
    const planned = groups.flatMap((g) => {
      const rows = [{ path: g.name.trim(), existingId: g.existingId }];
      for (const c of g.children) {
        if (g.name.trim() && c.name.trim()) {
          rows.push({ path: `${g.name.trim()}/${c.name.trim()}`, existingId: c.existingId });
        }
      }
      return rows;
    });
    const targetsToWrite = planned.filter((n) => n.path);
    const toCreate = targetsToWrite.filter((n) => !n.existingId);
    const reused = targetsToWrite.length - toCreate.length;
    const where =
      target === 'tag'
        ? `标签「${run.dimension}」前缀下`
        : scope.collectionId
          ? `集合「${scope.label}」下`
          : `新建的根集合「AI 分类 · ${run.dimension}」下`;
    const ok = await confirm(
      `将${toCreate.length > 0 ? `新建 ${toCreate.length} 个` : '不新建'}${
        target === 'tag' ? '标签' : '集合'
      }${reused > 0 ? `、复用 ${reused} 个已有分类` : ''}，放在${where}，\n并为 ${
        accepted.length
      } 篇文献建立归属。${pending.length > 0 ? `\n\n还有 ${pending.length} 篇待审，本次不会写入。` : ''}`
    );
    if (!ok) return;
    setBusy(true);
    try {
      await persistGroups();
      await syncItems();
      const summary = await aiClassifyApply(id, {
        targets: targetsToWrite.map((n) => ({ groupName: n.path, create: !n.existingId })),
        parentCollectionId: target === 'collection' ? scope.collectionId : null,
      });
      setApplySummary(summary);
      setStep('result');
      queryClient.invalidateQueries({ queryKey: ['collections'] });
      queryClient.invalidateQueries({ queryKey: ['tags'] });
      queryClient.invalidateQueries({ queryKey: ['papers'] });
      void loadVocab(run.dimension, run.target);
    } catch (err) {
      await alert(`写回失败: ${err}`);
    } finally {
      setBusy(false);
    }
  };

  const undo = async () => {
    const id = runIdRef.current;
    if (!id) return;
    if (
      !(await confirm(
        '撤销本次分类？本轮新建的类目与全部归属都会被移除（其他轮次和手动整理的保持不变）。'
      ))
    )
      return;
    setBusy(true);
    try {
      const summary = await aiClassifyUndo(id);
      setUndoSummary(summary);
      queryClient.invalidateQueries({ queryKey: ['collections'] });
      queryClient.invalidateQueries({ queryKey: ['tags'] });
      queryClient.invalidateQueries({ queryKey: ['papers'] });
    } catch (err) {
      await alert(`撤销失败: ${err}`);
    } finally {
      setBusy(false);
    }
  };

  /** Export the current run as a research report (CSV / Markdown). Works in
   *  仅预览 mode too — that is the "read the result without writing the library"
   *  path. */
  const exportReport = async (format: 'csv' | 'md') => {
    if (!run) return;
    const header = ['标题', '年份', '归属', '置信度', '依据', '状态', '手动'];
    const rows = items.map((it) => [
      it.title,
      it.year ?? '',
      it.labels.join(' / '),
      it.confidence > 0 ? `${Math.round(it.confidence * 100)}%` : '',
      it.reason,
      it.labels.length === 0 ? '待定' : it.status === 'pending' && !it.edited ? '待审' : '已接受',
      it.edited ? '是' : '',
    ]);
    const stamp = new Date().toISOString().slice(0, 16).replace(/[:T]/g, '-');
    const base = `ai-分类-${run.dimension}-${stamp}`;
    try {
      const { save } = await import('@tauri-apps/plugin-dialog');
      if (format === 'csv') {
        const esc = (v: string | number) => `"${String(v).replace(/"/g, '""')}"`;
        const csv = [header, ...rows].map((r) => r.map(esc).join(',')).join('\r\n');
        const target = await save({
          defaultPath: `${base}.csv`,
          filters: [{ name: 'CSV', extensions: ['csv'] }],
        });
        // BOM so Excel opens CJK correctly.
        if (target) await saveTextFile(target, '\ufeff' + csv);
      } else {
        const esc = (v: string | number) => String(v).replace(/\|/g, '\\|');
        const md = [
          `# AI 分类报告 · ${run.dimension}`,
          '',
          `- 维度：${run.dimension}`,
          `- 目标：${run.target === 'tag' ? '标签' : '集合'}`,
          `- 门槛：${Math.round(run.threshold * 100)}%`,
          `- 类目：${paths.join('、')}`,
          `- 统计：已接受 ${accepted.length} · 待审 ${pending.length} · 待定 ${unassigned.length}`,
          '',
          `| ${header.join(' | ')} |`,
          `|${header.map(() => '---').join('|')}|`,
          ...rows.map((r) => `| ${r.map(esc).join(' | ')} |`),
        ].join('\n');
        const target = await save({
          defaultPath: `${base}.md`,
          filters: [{ name: 'Markdown', extensions: ['md'] }],
        });
        if (target) await saveTextFile(target, md);
      }
    } catch (err) {
      await alert(`导出失败: ${err}`);
    }
  };

  /** Run the same paper set under another dimension (the multi-dimension path:
   *  each dimension is its own run so the vocabularies never mix). */
  const continueWith = async (nextDimension: string) => {
    setDimension(nextDimension);
    setCustomDimension('');
    setApplySummary(null);
    setUndoSummary(null);
    setStep('config');
    await start(nextDimension);
  };

  const setVocabStatus = async (ids: string[], status: 'active' | 'staged' | 'rejected') => {
    try {
      const affected = vocab.filter((v) => ids.includes(v.id)).map((v) => v.path);
      await aiClassifyVocabSetStatus(ids, status);
      if (run) await loadVocab(run.dimension, run.target);
      if (status === 'rejected') {
        // Rejected terms are negative memory: drop them from the draft too.
        setGroups((prev) =>
          prev
            .filter((g) => !affected.includes(g.name))
            .map((g) => ({
              ...g,
              children: g.children.filter((c) => !affected.includes(`${g.name}/${c.name}`)),
            }))
        );
      }
    } catch (err) {
      await alert(`更新词表失败: ${err}`);
    }
  };

  if (!open) return null;

  const runningLabel = progress
    ? progress.stage === 'merge'
      ? '合并近义类目…'
      : progress.stage === 'hierarchize'
        ? '生成二级类目…'
        : progress.stage === 'assign'
          ? `判定中 ${progress.done}/${progress.total}`
          : `起草中 ${progress.done}/${progress.total}`
    : '准备中…';

  const visibleItems = tab === 'pending' ? pending : tab === 'accepted' ? accepted : items;

  return (
    <div className="fixed inset-0 z-[6000] flex items-center justify-center">
      {/* Closing is always allowed; the backend run reports its own state and
          停止 now interrupts an in-flight model call. */}
      <div className="absolute inset-0 bg-black/50 backdrop-blur-sm" onClick={close} />
      <div className="relative w-[94vw] max-w-[1240px] h-[86vh] mx-4 flex flex-col bg-surface border border-surface-hover rounded-xl shadow-2xl overflow-hidden">
        {/* Header */}
        <div className="flex items-center gap-2 px-4 py-2.5 border-b border-surface-hover shrink-0">
          <Sparkles size={15} className="text-primary" />
          <span className="text-sm font-medium text-text-primary">AI 文献分类</span>
          <span className="text-[11px] text-text-secondary/60">
            {scope.label} · {papers} 篇
          </span>
          <span className="ml-2 flex items-center gap-1 text-[11px] text-text-secondary/50">
            <span className={step === 'config' ? 'text-primary' : ''}>1 配置</span>
            <ChevronDown size={10} className="-rotate-90" />
            <span className={step === 'review' ? 'text-primary' : ''}>2 草案与落位</span>
            <ChevronDown size={10} className="-rotate-90" />
            <span className={step === 'result' ? 'text-primary' : ''}>3 应用</span>
          </span>
          <div className="ml-auto flex items-center gap-2">
            {running && (
              <button
                onClick={() => void aiClassifyCancel()}
                className="flex items-center gap-1 px-2.5 py-1.5 rounded-lg border border-surface-hover text-[12px] text-red-400 hover:bg-surface-hover"
              >
                <Square size={12} />
                停止
              </button>
            )}
            <button
              onClick={close}
              title={running || busy ? '关闭窗口不会中断任务（可用「停止」取消）' : '关闭'}
              className="p-1.5 rounded text-text-secondary/60 hover:text-text-primary hover:bg-surface-hover"
            >
              <X size={15} />
            </button>
          </div>
        </div>

        {/* Step 1: config */}
        {step === 'config' && (
          <div className="flex-1 min-h-0 flex">
            <div className="flex-1 min-w-0 min-h-0 overflow-y-auto p-5">
              <div className="max-w-[680px] space-y-4">
              <div>
                <label className="block text-[12px] text-text-secondary mb-1.5">分类维度</label>
                <div className="flex flex-wrap gap-1.5">
                  {DIMENSIONS.map((d) => (
                    <button
                      key={d}
                      onClick={() => setDimension(d)}
                      className={`px-2.5 py-1 rounded-lg border text-[12px] transition-colors ${
                        dimension === d
                          ? 'border-primary/50 bg-primary/10 text-primary'
                          : 'border-surface-hover text-text-secondary hover:bg-surface-hover'
                      }`}
                    >
                      {d}
                    </button>
                  ))}
                  <button
                    onClick={() => setDimension('自定义')}
                    className={`px-2.5 py-1 rounded-lg border text-[12px] transition-colors ${
                      dimension === '自定义'
                        ? 'border-primary/50 bg-primary/10 text-primary'
                        : 'border-surface-hover text-text-secondary hover:bg-surface-hover'
                    }`}
                  >
                    自定义
                  </button>
                </div>
                {dimension === '自定义' && (
                  <input
                    value={customDimension}
                    onChange={(e) => setCustomDimension(e.target.value)}
                    placeholder="如：理论视角、实验对象、应用场景"
                    className="mt-2 w-full bg-background border border-surface-hover rounded px-2 py-1.5 text-[12px] text-text-primary outline-none focus:border-primary/50"
                  />
                )}
                <p className="text-[11px] text-text-secondary/50 mt-1.5">
                  维度决定「怎么分」：同一批文献可以按主题分一次、再按方法分一次，互不干扰。维度也是词表的键——同维度的分类会越用越收敛。
                </p>
              </div>

              <div>
                <label className="block text-[12px] text-text-secondary mb-1.5">归类目标</label>
                <div className="grid grid-cols-3 gap-2">
                  {(
                    [
                      {
                        key: 'collection',
                        label: '集合（分类）',
                        icon: <FolderTree size={13} />,
                        hint: '单归属，左侧目录树',
                      },
                      { key: 'tag', label: '标签', icon: <Tags size={13} />, hint: '多标签（最多 3 个）' },
                      { key: 'preview', label: '仅预览', icon: <Eye size={13} />, hint: '只看结果，不写回' },
                    ] as const
                  ).map((opt) => (
                    <button
                      key={opt.key}
                      onClick={() => setTarget(opt.key)}
                      className={`flex flex-col items-start gap-0.5 px-2.5 py-2 rounded-lg border text-left transition-colors ${
                        target === opt.key
                          ? 'border-primary/50 bg-primary/10'
                          : 'border-surface-hover hover:bg-surface-hover'
                      }`}
                    >
                      <span
                        className={`flex items-center gap-1.5 text-[12px] ${
                          target === opt.key ? 'text-primary' : 'text-text-primary'
                        }`}
                      >
                        {opt.icon}
                        {opt.label}
                      </span>
                      <span className="text-[10px] text-text-secondary/50">{opt.hint}</span>
                    </button>
                  ))}
                </div>
              </div>

              <div className="grid grid-cols-2 gap-3">
                <div className="rounded-lg border border-surface-hover/70 p-2.5 space-y-1.5">
                  <label className="flex items-center gap-2 text-[12px] text-text-primary">
                    <input
                      type="checkbox"
                      checked={sampleFirst}
                      onChange={(e) => setSampleFirst(e.target.checked)}
                      className="accent-primary"
                    />
                    先跑样例
                  </label>
                  <p className="text-[11px] text-text-secondary/50">
                    只起草前 {Math.min(sampleSize, papers)} 篇
                    {remainingHint(papers, sampleFirst, sampleSize)}
                  </p>
                  {sampleFirst && (
                    <div className="flex gap-1.5">
                      {SAMPLE_SIZES.map((n) => (
                        <button
                          key={n}
                          onClick={() => setSampleSize(n)}
                          className={`px-2 py-0.5 rounded border text-[11px] ${
                            sampleSize === n
                              ? 'border-primary/50 bg-primary/10 text-primary'
                              : 'border-surface-hover text-text-secondary hover:bg-surface-hover'
                          }`}
                        >
                          {n} 篇
                        </button>
                      ))}
                    </div>
                  )}
                  <label className="flex items-center gap-2 text-[12px] text-text-primary pt-1">
                    <input
                      type="checkbox"
                      checked={twoLevel}
                      onChange={(e) => setTwoLevel(e.target.checked)}
                      className="accent-primary"
                    />
                    两级类目
                  </label>
                  <p className="text-[11px] text-text-secondary/50">
                    多花 1 次调用，让 AI 把类目归纳成 父类 / 子类
                  </p>
                </div>

                <div className="rounded-lg border border-surface-hover/70 p-2.5 space-y-1.5">
                  <label className="flex items-center gap-2 text-[12px] text-text-primary">
                    <input
                      type="checkbox"
                      checked={reuseExisting}
                      onChange={(e) => setReuseExisting(e.target.checked)}
                      className="accent-primary"
                    />
                    复用库中已有分类
                  </label>
                  <p className="text-[11px] text-text-secondary/50">
                    同名类目直接归入已有{target === 'tag' ? '标签' : '集合'}，不重复新建
                  </p>
                  <label className="flex items-center gap-2 text-[12px] text-text-primary pt-1">
                    <input
                      type="checkbox"
                      checked={skipExisting}
                      onChange={(e) => setSkipExisting(e.target.checked)}
                      className="accent-primary"
                    />
                    跳过已分类文献
                  </label>
                  <p className="text-[11px] text-text-secondary/50">
                    已按「{dimensionValue || '该维度'}」归过类的文献不再重复判定
                  </p>
                  <div className="flex items-center gap-2 text-[11px] text-text-secondary/70 pt-1">
                    类目数上限
                    <input
                      type="number"
                      min={3}
                      max={30}
                      value={maxGroups}
                      onChange={(e) =>
                        setMaxGroups(Math.max(3, Math.min(30, Number(e.target.value) || 12)))
                      }
                      className="w-16 bg-background border border-surface-hover rounded px-1.5 py-0.5 text-[11px] text-text-primary outline-none focus:border-primary/50"
                    />
                  </div>
                </div>
              </div>

              <div className="rounded-lg border border-surface-hover/70 p-2.5 space-y-1.5">
                <label className="block text-[11px] text-text-secondary/60">
                  置信度门槛（低于此值进「待审」）
                </label>
                <div className="flex flex-wrap gap-1.5">
                  {THRESHOLDS.map((t) => (
                    <button
                      key={t.value}
                      onClick={() => setThreshold(t.value)}
                      className={`px-2 py-0.5 rounded border text-[11px] ${
                        threshold === t.value
                          ? 'border-primary/50 bg-primary/10 text-primary'
                          : 'border-surface-hover text-text-secondary hover:bg-surface-hover'
                      }`}
                    >
                      {t.label}
                    </button>
                  ))}
                </div>
                <p className="text-[11px] text-text-secondary/50">
                  低于门槛的判定不会自动写入，需在「待审」里确认；门槛越低越省事、越容易写错。
                </p>
              </div>

              <div className="rounded-lg border border-surface-hover/70 p-2.5 space-y-1.5">
                <label className="block text-[11px] text-text-secondary/60">起草引擎</label>
                <div className="flex flex-wrap gap-1.5">
                  <button
                    onClick={() => setEngine('batch')}
                    className={`flex items-center gap-1 px-2 py-0.5 rounded border text-[11px] ${
                      engine === 'batch'
                        ? 'border-primary/50 bg-primary/10 text-primary'
                        : 'border-surface-hover text-text-secondary hover:bg-surface-hover'
                    }`}
                  >
                    批量起草（逐批读摘要）
                  </button>
                  <button
                    onClick={() => capabilities?.semantic && setEngine('semantic')}
                    disabled={!capabilities?.semantic}
                    title={
                      capabilities?.semantic
                        ? '在向量空间聚类，只需 1 次模型调用'
                        : '需要先在设置里配置语义向量（embeddings）端点'
                    }
                    className={`flex items-center gap-1 px-2 py-0.5 rounded border text-[11px] disabled:opacity-40 ${
                      engine === 'semantic'
                        ? 'border-primary/50 bg-primary/10 text-primary'
                        : 'border-surface-hover text-text-secondary hover:bg-surface-hover'
                    }`}
                  >
                    <Cpu size={10} />
                    语义聚类（便宜）
                  </button>
                </div>
                <p className="text-[11px] text-text-secondary/50">
                  {engine === 'semantic'
                    ? '按全文向量聚类后只做 1 次命名调用；聚类质量取决于向量后端，簇名仍可手改。'
                    : capabilities?.semantic
                      ? '逐批读标题+摘要起草，稳定但调用次数随篇数增长。'
                      : '语义聚类未启用：设置 → 高级 → 语义搜索 配置向量端点后可用。'}
                </p>
              </div>

              <div className="rounded-lg border border-surface-hover/70 p-2.5 space-y-1.5">
                <label className="block text-[11px] text-text-secondary/60">模型</label>
                <ModelOverrideSelect
                  value={providerId}
                  onChange={setProviderId}
                  disabled={running}
                  className="w-full"
                />
                <p className="text-[11px] text-text-secondary/50">
                  预估：起草约 {est.draftCalls} 次调用{twoLevel ? '（含二级归纳）' : ''}
                  {sampleFirst ? `，覆盖 ${est.drafted} 篇` : ''}；全量判定约 {est.fullAssignCalls} 次。
                  {papers > 200 ? ' 大库耗时可能到小时级，建议先跑样例。' : ''}
                </p>
              </div>

              <div className="flex items-center gap-2 pt-1">
                <button
                  onClick={() => void start()}
                  disabled={running}
                  className="flex items-center gap-1.5 px-3 py-1.5 rounded-lg bg-primary text-black text-[12px] font-medium hover:bg-primary/90 disabled:opacity-50"
                >
                  {running ? <Loader2 size={12} className="animate-spin" /> : <Play size={12} />}
                  {running ? runningLabel : '开始起草分类'}
                </button>
                <span className="text-[11px] text-text-secondary/50">
                  起草不会改动你的图书馆；确认草案后才会写回。
                </span>
              </div>
              </div>
            </div>

            {/* Right column: the run itself. Empty space next to a 680px form in
                a 1240px dialog told the user nothing while a multi-batch draft
                ran; this panel reports every beat. */}
            <aside className="w-[340px] shrink-0 min-h-0 flex flex-col border-l border-surface-hover">
              <div className="flex items-center gap-1.5 px-3 py-2.5 border-b border-surface-hover shrink-0">
                <Activity
                  size={13}
                  className={running ? 'text-primary' : 'text-text-secondary/50'}
                />
                <span className="text-[12px] font-medium text-text-primary">
                  {running ? '生成过程' : '本次运行'}
                </span>
                {running && <Loader2 size={11} className="animate-spin text-primary" />}
                {!running && progress?.finished === 'error' && (
                  <span className="ml-auto text-[10px] text-red-400">上次起草失败</span>
                )}
                {!running && progress?.finished === 'done' && (
                  <span className="ml-auto text-[10px] text-emerald-400">起草完成</span>
                )}
              </div>

              <div className="flex-1 min-h-0 overflow-y-auto px-3 py-2.5 space-y-3 text-[11px]">
                {running ? (
                  <>
                    {/* Stage stepper: which phase the pipeline is in right now. */}
                    <div className="flex flex-wrap items-center gap-1">
                      {[
                        { key: 'draft', label: '起草' },
                        { key: 'merge', label: '合并' },
                        { key: 'hierarchize', label: '二级归纳' },
                      ].map((s, i) => {
                        const active = progress?.stage === s.key;
                        const passed =
                          progress != null &&
                          ['draft', 'merge', 'hierarchize'].indexOf(progress.stage) > i;
                        return (
                          <span
                            key={s.key}
                            className={`px-1.5 py-0.5 rounded ${
                              active
                                ? 'bg-primary/20 text-primary'
                                : passed
                                  ? 'bg-emerald-500/15 text-emerald-400'
                                  : 'bg-surface-hover text-text-secondary/50'
                            }`}
                          >
                            {s.label}
                          </span>
                        );
                      })}
                    </div>

                    {progress && (
                      <div className="space-y-1">
                        <div className="flex items-center justify-between text-text-secondary/70">
                          <span className="truncate">{progress.current || runningLabel}</span>
                          <span className="tabular-nums shrink-0 ml-2">
                            {progress.done}/{progress.total}
                            {progress.failed > 0 && (
                              <span className="text-red-400"> · 失败 {progress.failed}</span>
                            )}
                          </span>
                        </div>
                        <div className="h-1 rounded bg-surface-hover overflow-hidden">
                          <div
                            className="h-full bg-primary transition-all"
                            style={{
                              width: `${
                                progress.total > 0
                                  ? Math.min(100, Math.round((progress.done / progress.total) * 100))
                                  : 0
                              }%`,
                            }}
                          />
                        </div>
                      </div>
                    )}

                    {progress?.groupsPreview && progress.groupsPreview.length > 0 && (
                      <div className="space-y-1">
                        <p className="text-text-secondary/50">
                          已提出的类目（{progress.groupsPreview.length}）
                        </p>
                        <div className="flex flex-wrap gap-1">
                          {progress.groupsPreview.map((g) => (
                            <span
                              key={g}
                              className="px-1.5 py-0.5 rounded bg-primary/10 text-primary max-w-full truncate"
                              title={g}
                            >
                              {g}
                            </span>
                          ))}
                        </div>
                      </div>
                    )}

                    <div className="space-y-1">
                      <p className="text-text-secondary/50">过程日志</p>
                      <div
                        ref={logRef}
                        className="rounded border border-surface-hover/60 px-2 py-1.5 font-mono text-[10px] leading-relaxed text-text-secondary/70 space-y-0.5 max-h-[220px] overflow-y-auto"
                      >
                        {logs.length === 0 ? (
                          <span className="text-text-secondary/40">等待第一批结果…</span>
                        ) : (
                          logs.slice(-40).map((l, i) => <div key={i}>{l}</div>)
                        )}
                      </div>
                    </div>
                  </>
                ) : (
                  <>
                    <dl className="space-y-1.5">
                      {[
                        { k: '范围', v: `${scope.label} · ${papers} 篇` },
                        { k: '维度', v: dimensionValue || '—' },
                        {
                          k: '写入',
                          v:
                            target === 'tag'
                              ? `标签 ${dimensionValue || ''}#…`
                              : scope.collectionId
                                ? `集合「${scope.label}」下`
                                : `新建根集合 AI 分类 · ${dimensionValue || ''}`,
                        },
                        {
                          k: '引擎',
                          v: engine === 'semantic' ? '语义聚类' : '批量起草',
                        },
                        {
                          k: '样例',
                          v: sampleFirst ? `前 ${Math.min(sampleSize, papers)} 篇` : '全量',
                        },
                        {
                          k: '预估',
                          v: `约 ${est.draftCalls} 次起草 + ${est.fullAssignCalls} 次判定`,
                        },
                      ].map((row) => (
                        <div key={row.k} className="flex gap-2">
                          <dt className="w-[42px] shrink-0 text-text-secondary/50">{row.k}</dt>
                          <dd className="flex-1 min-w-0 text-text-primary/85 break-words">{row.v}</dd>
                        </div>
                      ))}
                    </dl>

                    <div className="rounded border border-surface-hover/60 px-2 py-1.5 space-y-1">
                      <p className="text-text-secondary/50">
                        词表 · {dimensionValue || '当前维度'}
                      </p>
                      {vocabStats ? (
                        <p className="text-text-primary/85">
                          已采纳 {vocabStats.active} · 待采纳 {vocabStats.staged}
                          {vocabStats.rejected > 0 && ` · 已丢弃 ${vocabStats.rejected}`}
                        </p>
                      ) : (
                        <p className="text-text-secondary/40">暂无词表条目</p>
                      )}
                      <p className="text-text-secondary/45 leading-snug">
                        已采纳的类目会作为下次起草的优先词表；丢弃的不会再被提议。
                      </p>
                    </div>

                    {progress?.finished === 'error' && logs.length > 0 && (
                      <div className="rounded border border-red-500/30 bg-red-500/5 px-2 py-1.5 space-y-1">
                        <p className="text-red-400">上次起草失败</p>
                        <div className="font-mono text-[10px] text-text-secondary/70 max-h-[160px] overflow-y-auto space-y-0.5">
                          {logs.slice(-12).map((l, i) => (
                            <div key={i}>{l}</div>
                          ))}
                        </div>
                      </div>
                    )}

                    <p className="text-text-secondary/45 leading-snug">
                      起草阶段只读不写：类目在草案里等你确认后才会落到图书馆。
                    </p>
                  </>
                )}
              </div>
            </aside>
          </div>
        )}

        {/* Step 2: review */}
        {step === 'review' && run && (
          <div className="flex-1 min-h-0 flex">
            {/* Left: editable taxonomy draft */}
            <aside className="w-[320px] shrink-0 border-r border-surface-hover flex flex-col">
              <div className="px-3 py-2.5 border-b border-surface-hover">
                <div className="flex items-center justify-between">
                  <span className="text-[13px] font-medium text-text-primary">分类草案</span>
                  <span className="text-[11px] text-text-secondary/60">
                    {groups.length} 组 · {paths.length} 类目
                  </span>
                </div>
                <p className="text-[11px] text-text-secondary/60 mt-0.5">
                  可改名 / 加子类 / 删除；改动会带到判定与应用
                </p>
              </div>
              <div className="flex-1 overflow-y-auto p-2.5 space-y-2">
                {groups.map((g) => {
                  const path = g.name.trim();
                  const isCollapsed = collapsed.has(g.name);
                  return (
                    <div
                      key={`g-${g.name}`}
                      className="rounded-lg border border-surface-hover/70 p-2 space-y-1.5"
                    >
                      <GroupNameInput
                        value={g.name}
                        placeholder="类目名"
                        onLocalChange={(v) => renameLocal(path, v)}
                        onCommit={(oldV, v) => void commitRename(oldV || path, v)}
                        onRemove={() => void removeGroup(path)}
                      />
                      <div className="flex items-center gap-1.5 text-[10px]">
                        <span
                          className={`px-1.5 py-0.5 rounded ${
                            g.existingId
                              ? 'bg-emerald-500/15 text-emerald-400'
                              : 'bg-primary/15 text-primary'
                          }`}
                        >
                          {g.existingId ? '复用已有' : '新建'}
                        </span>
                        {g.children.length > 0 && (
                          <button
                            onClick={() =>
                              setCollapsed((prev) => {
                                const next = new Set(prev);
                                if (next.has(g.name)) next.delete(g.name);
                                else next.add(g.name);
                                return next;
                              })
                            }
                            className="flex items-center gap-0.5 text-text-secondary/60 hover:text-text-primary"
                          >
                            {isCollapsed ? <ChevronRight size={10} /> : <ChevronDown size={10} />}
                            {g.children.length} 子类
                          </button>
                        )}
                        <PathCounts counts={countsFor(items, path)} />
                      </div>
                      {g.description && (
                        <p className="text-[10px] text-text-secondary/50 leading-snug">
                          {g.description}
                        </p>
                      )}

                      {!isCollapsed && g.children.length > 0 && (
                        <div className="space-y-1.5 border-l border-surface-hover/60 pl-2 ml-1">
                          {g.children.map((c) => {
                            const childPath = `${path}/${c.name.trim()}`;
                            return (
                              <div
                                key={`c-${c.name}`}
                                className="rounded border border-surface-hover/50 p-1.5 space-y-1"
                              >
                                <GroupNameInput
                                  value={c.name}
                                  placeholder="子类名"
                                  compact
                                  onLocalChange={(v) => renameLocal(childPath, v)}
                                  onCommit={(oldV, v) => void commitRename(oldV || childPath, v)}
                                  onRemove={() => void removeGroup(childPath)}
                                />
                                <div className="flex items-center gap-1.5 text-[10px]">
                                  <span
                                    className={`px-1 py-0.5 rounded ${
                                      c.existingId
                                        ? 'bg-emerald-500/15 text-emerald-400'
                                        : 'bg-primary/15 text-primary'
                                    }`}
                                  >
                                    {c.existingId ? '复用' : '新建'}
                                  </span>
                                  <PathCounts counts={countsFor(items, childPath)} />
                                </div>
                              </div>
                            );
                          })}
                        </div>
                      )}

                      {path && (
                        <button
                          onClick={() => void addGroup(path)}
                          className="flex items-center gap-1 text-[10px] text-text-secondary/60 hover:text-primary"
                        >
                          <Plus size={10} />
                          加子类
                        </button>
                      )}
                    </div>
                  );
                })}
                <button
                  onClick={() => void addGroup(null)}
                  className="w-full flex items-center justify-center gap-1 px-2 py-1.5 rounded-lg border border-dashed border-surface-hover text-[12px] text-text-secondary hover:bg-surface-hover"
                >
                  <Plus size={12} />
                  添加类目
                </button>
              </div>
              <div className="p-2.5 border-t border-surface-hover space-y-1.5">
                <button
                  onClick={() => void reassign(remaining > 0)}
                  disabled={running || paths.length === 0}
                  className="w-full flex items-center justify-center gap-1.5 px-2 py-1.5 rounded-lg bg-primary text-black text-[12px] font-medium hover:bg-primary/90 disabled:opacity-50"
                >
                  {running ? <Loader2 size={12} className="animate-spin" /> : <RefreshCw size={12} />}
                  {running ? runningLabel : remaining > 0 ? `判定剩余 ${remaining} 篇` : '按草案重新判定'}
                </button>
                {remaining === 0 && !running && (
                  <button
                    onClick={() => void reassign(false)}
                    className="w-full px-2 py-1.5 rounded-lg border border-surface-hover text-[12px] text-text-secondary hover:bg-surface-hover"
                  >
                    全部重判（含已接受）
                  </button>
                )}
                <button
                  onClick={() => {
                    setVocabOpen(true);
                    void loadVocab(run.dimension, run.target);
                  }}
                  className="w-full flex items-center justify-center gap-1.5 px-2 py-1.5 rounded-lg border border-surface-hover text-[12px] text-text-secondary hover:bg-surface-hover"
                >
                  <BookMarked size={12} />
                  查看词表
                  {vocab.filter((v) => v.status === 'staged').length > 0
                    ? `（${vocab.filter((v) => v.status === 'staged').length} 待采纳）`
                    : ''}
                </button>
              </div>
            </aside>

            {/* Right: per-paper assignment */}
            <div className="flex-1 min-w-0 flex flex-col">
              <div className="flex items-center gap-2 px-4 py-2 border-b border-surface-hover text-[11px] text-text-secondary/70">
                {(
                  [
                    { key: 'all', label: `全部 ${items.length}` },
                    { key: 'pending', label: `待审 ${pending.length}` },
                    { key: 'accepted', label: `已接受 ${accepted.length}` },
                  ] as const
                ).map((t) => (
                  <button
                    key={t.key}
                    onClick={() => setTab(t.key)}
                    className={`px-2 py-0.5 rounded ${
                      tab === t.key ? 'bg-primary/15 text-primary' : 'hover:bg-surface-hover'
                    }`}
                  >
                    {t.label}
                  </button>
                ))}
                {unassigned.length > 0 && (
                  <span className="text-text-secondary/50">待定 {unassigned.length}</span>
                )}
                {remaining > 0 && (
                  <span className="text-text-secondary/50">· 还有 {remaining} 篇未起草</span>
                )}
                <div className="ml-auto flex items-center gap-1.5">
                  {tab === 'pending' && pending.length > 0 && (
                    <>
                      <button
                        onClick={() => void resolvePending(true)}
                        className="px-2 py-0.5 rounded bg-primary/15 text-primary hover:bg-primary/25"
                      >
                        全部接受
                      </button>
                      <button
                        onClick={() => void resolvePending(false)}
                        className="px-2 py-0.5 rounded hover:bg-surface-hover"
                      >
                        全部标为待定
                      </button>
                    </>
                  )}
                  <button
                    onClick={() => void exportReport('csv')}
                    disabled={items.length === 0}
                    className="flex items-center gap-0.5 px-2 py-0.5 rounded hover:bg-surface-hover disabled:opacity-40"
                    title="导出当前判定结果为 CSV"
                  >
                    <Download size={10} />
                    CSV
                  </button>
                  <button
                    onClick={() => void exportReport('md')}
                    disabled={items.length === 0}
                    className="flex items-center gap-0.5 px-2 py-0.5 rounded hover:bg-surface-hover disabled:opacity-40"
                    title="导出分类报告（Markdown）"
                  >
                    <Download size={10} />
                    报告
                  </button>
                  <button
                    onClick={() => setShowLog((v) => !v)}
                    className={`px-2 py-0.5 rounded ${
                      showLog ? 'text-primary bg-primary/10' : 'hover:bg-surface-hover'
                    }`}
                  >
                    过程日志 {logs.length > 0 ? `(${logs.length})` : ''}
                  </button>
                </div>
              </div>

              <div className="flex-1 min-h-0 flex">
                <div className="flex-1 min-w-0 overflow-auto">
                  <table className="w-full border-collapse text-[12px]">
                    <thead className="sticky top-0 bg-surface z-10">
                      <tr>
                        <th className="text-left px-3 py-1.5 border-b border-surface-hover text-text-secondary font-medium min-w-[210px]">
                          标题
                        </th>
                        <th className="text-left px-2 py-1.5 border-b border-surface-hover text-text-secondary font-medium w-[210px]">
                          归属{multiLabel ? '（多标签）' : ''}
                        </th>
                        <th className="text-left px-2 py-1.5 border-b border-surface-hover text-text-secondary font-medium w-[62px]">
                          置信度
                        </th>
                        <th className="text-left px-2 py-1.5 border-b border-surface-hover text-text-secondary font-medium min-w-[110px]">
                          依据
                        </th>
                        <th className="text-left px-2 py-1.5 border-b border-surface-hover text-text-secondary font-medium w-[66px]">
                          状态
                        </th>
                      </tr>
                    </thead>
                    <tbody>
                      {visibleItems.map((it) => (
                        <tr
                          key={it.paperId}
                          className="border-b border-surface-hover/50 hover:bg-surface-hover/30"
                        >
                          <td className="px-3 py-1.5 align-top">
                            <span className="block text-text-primary line-clamp-2">{it.title}</span>
                            {it.year && (
                              <span className="text-[10px] text-text-secondary/50">{it.year}</span>
                            )}
                          </td>
                          <td className="px-2 py-1.5 align-top">
                            {multiLabel ? (
                              <LabelChips
                                labels={it.labels}
                                paths={paths}
                                onRemove={(label) =>
                                  void moveItem(
                                    it.paperId,
                                    it.labels.filter((l) => l !== label)
                                  )
                                }
                                onAdd={(label) => void moveItem(it.paperId, [...it.labels, label])}
                              />
                            ) : (
                              <select
                                value={it.labels[0] ?? ''}
                                onChange={(e) =>
                                  void moveItem(it.paperId, e.target.value ? [e.target.value] : [])
                                }
                                className={`w-full bg-background border rounded px-1.5 py-1 text-[11px] outline-none focus:border-primary/50 ${
                                  it.labels.length
                                    ? 'border-surface-hover text-text-primary'
                                    : 'border-amber-500/40 text-amber-400'
                                }`}
                              >
                                <option value="">待定</option>
                                {paths.map((p) => (
                                  <option key={p} value={p}>
                                    {p}
                                  </option>
                                ))}
                              </select>
                            )}
                            {it.edited && <span className="text-[10px] text-primary/70">手动</span>}
                          </td>
                          <td className="px-2 py-1.5 align-top text-[11px] text-text-secondary/70 tabular-nums">
                            {it.confidence > 0 ? `${Math.round(it.confidence * 100)}%` : '—'}
                          </td>
                          <td className="px-2 py-1.5 align-top text-[11px] text-text-secondary/60 leading-snug">
                            {it.reason || '—'}
                          </td>
                          <td className="px-2 py-1.5 align-top">
                            <StatusBadge item={it} />
                          </td>
                        </tr>
                      ))}
                      {visibleItems.length === 0 && (
                        <tr>
                          <td colSpan={5} className="px-3 py-8 text-center text-text-secondary/50">
                            {tab === 'pending' ? '没有待审的文献' : '还没有判定结果'}
                          </td>
                        </tr>
                      )}
                    </tbody>
                  </table>
                </div>

                {showLog && (
                  <div
                    ref={logRef}
                    className="w-[230px] shrink-0 border-l border-surface-hover overflow-y-auto px-2.5 py-2 font-mono text-[10px] leading-relaxed text-text-secondary/70 space-y-1"
                  >
                    {logs.length === 0 ? (
                      <span className="text-text-secondary/40">暂无日志</span>
                    ) : (
                      logs.map((l, i) => <div key={i}>{l}</div>)
                    )}
                  </div>
                )}
              </div>

              {/* Footer */}
              <div className="flex items-center gap-2 px-4 py-2 border-t border-surface-hover shrink-0">
                <span className="text-[11px] text-text-secondary/60">
                  {target === 'preview'
                    ? '仅预览模式：不会写回图书馆'
                    : `将写回${
                        target === 'tag'
                          ? `标签「${run.dimension}#…」`
                          : scope.collectionId
                            ? `集合「${scope.label}」下`
                            : `新建根集合「AI 分类 · ${run.dimension}」`
                      }`}
                  {pending.length > 0 && ` · ${pending.length} 篇待审暂不写入`}
                </span>
                <div className="ml-auto flex items-center gap-2">
                  {target === 'preview' && (
                    <button
                      onClick={() => setTarget('collection')}
                      className="px-2.5 py-1.5 rounded-lg border border-surface-hover text-[12px] text-text-secondary hover:bg-surface-hover"
                    >
                      改为写入集合
                    </button>
                  )}
                  <button
                    onClick={() => void apply()}
                    disabled={busy || running || target === 'preview' || accepted.length === 0}
                    className="flex items-center gap-1 px-3 py-1.5 rounded-lg bg-primary text-black text-[12px] font-medium hover:bg-primary/90 disabled:opacity-40"
                  >
                    {busy ? <Loader2 size={12} className="animate-spin" /> : <Check size={12} />}
                    应用分类（{accepted.length} 篇）
                  </button>
                </div>
              </div>
            </div>

            {/* Vocabulary drawer */}
            {vocabOpen && (
              <div className="absolute inset-0 z-10 flex justify-end">
                <div className="absolute inset-0 bg-black/30" onClick={() => setVocabOpen(false)} />
                <div className="relative w-[360px] h-full bg-surface border-l border-surface-hover flex flex-col">
                  <div className="flex items-center gap-2 px-3 py-2.5 border-b border-surface-hover">
                    <BookMarked size={13} className="text-primary" />
                    <span className="text-[13px] font-medium text-text-primary">词表</span>
                    <span className="text-[11px] text-text-secondary/60">
                      {run.dimension} · {run.target === 'tag' ? '标签' : '集合'}
                    </span>
                    <button
                      onClick={() => setVocabOpen(false)}
                      className="ml-auto p-1 rounded text-text-secondary/60 hover:text-text-primary hover:bg-surface-hover"
                    >
                      <X size={14} />
                    </button>
                  </div>
                  <p className="px-3 py-2 text-[11px] text-text-secondary/55 leading-snug">
                    采纳的类目会成为下次起草的优先词表；丢弃的类目不会再被 AI 提议。应用分类时用到的类目会自动采纳。
                  </p>
                  <div className="flex-1 overflow-y-auto px-3 pb-3 space-y-1.5">
                    {vocab.length === 0 && (
                      <p className="text-[11px] text-text-secondary/50 py-4 text-center">
                        还没有词表条目——起草或应用分类后会自动出现。
                      </p>
                    )}
                    {vocab.map((v) => (
                      <div
                        key={v.id}
                        className="flex items-center gap-1.5 rounded-lg border border-surface-hover/70 px-2 py-1.5"
                      >
                        <span
                          className="flex-1 min-w-0 truncate text-[12px] text-text-primary"
                          title={v.path}
                        >
                          {v.path}
                        </span>
                        <span
                          className={`px-1.5 py-0.5 rounded text-[10px] shrink-0 ${
                            v.status === 'active'
                              ? 'bg-emerald-500/15 text-emerald-400'
                              : v.status === 'rejected'
                                ? 'bg-red-500/15 text-red-400'
                                : 'bg-amber-500/15 text-amber-400'
                          }`}
                        >
                          {v.status === 'active' ? '已采纳' : v.status === 'rejected' ? '已丢弃' : '待采纳'}
                        </span>
                        {v.status !== 'active' && (
                          <button
                            onClick={() => void setVocabStatus([v.id], 'active')}
                            className="px-1.5 py-0.5 rounded text-[10px] text-emerald-400 hover:bg-surface-hover shrink-0"
                          >
                            采纳
                          </button>
                        )}
                        {v.status !== 'rejected' && (
                          <button
                            onClick={() => void setVocabStatus([v.id], 'rejected')}
                            className="px-1.5 py-0.5 rounded text-[10px] text-red-400 hover:bg-surface-hover shrink-0"
                          >
                            丢弃
                          </button>
                        )}
                        {v.status === 'rejected' && (
                          <button
                            onClick={() => void setVocabStatus([v.id], 'staged')}
                            className="px-1.5 py-0.5 rounded text-[10px] text-text-secondary hover:bg-surface-hover shrink-0"
                          >
                            恢复
                          </button>
                        )}
                      </div>
                    ))}
                  </div>
                </div>
              </div>
            )}
          </div>
        )}

        {/* Step 3: result */}
        {step === 'result' && (
          <div className="flex-1 min-h-0 overflow-y-auto p-6">
            <div className="max-w-[560px] space-y-4">
              {undoSummary ? (
                <div className="rounded-lg border border-surface-hover bg-surface-hover/30 p-4 space-y-1.5">
                  <p className="text-[13px] text-text-primary flex items-center gap-1.5">
                    <Undo2 size={14} className="text-amber-400" />
                    已撤销本次分类
                  </p>
                  <p className="text-[12px] text-text-secondary/70">
                    移除 {undoSummary.removedLinks} 条归属
                    {undoSummary.removedTargets > 0 &&
                      `、删除 ${undoSummary.removedTargets} 个本轮新建的类目`}
                    。被其他条目引用的类目会被保留。
                  </p>
                </div>
              ) : applySummary ? (
                <>
                  <div className="rounded-lg border border-emerald-500/30 bg-emerald-500/10 p-4 space-y-1.5">
                    <p className="text-[13px] text-text-primary flex items-center gap-1.5">
                      <Check size={14} className="text-emerald-400" />
                      已写回图书馆
                    </p>
                    <p className="text-[12px] text-text-secondary/80">
                      新建 {applySummary.createdTargets} 个类目、复用 {applySummary.reusedTargets}{' '}
                      个已有分类，为 {applySummary.linkedPapers} 篇文献建立归属
                      {applySummary.skippedPapers > 0 &&
                        `；${applySummary.skippedPapers} 篇未写入（待审或待定）`}
                      。
                    </p>
                  </div>
                  <div className="rounded-lg border border-surface-hover p-3.5 space-y-2">
                    <p className="text-[12px] text-text-primary">不满意？可以整轮撤销</p>
                    <p className="text-[11px] text-text-secondary/60">
                      撤销只会移除本轮新建的类目和本轮建立的归属；你手动整理的内容不受影响。
                    </p>
                    <button
                      onClick={() => void undo()}
                      disabled={busy}
                      className="flex items-center gap-1.5 px-3 py-1.5 rounded-lg border border-surface-hover text-[12px] text-amber-400 hover:bg-surface-hover disabled:opacity-40"
                    >
                      {busy ? <Loader2 size={12} className="animate-spin" /> : <Undo2 size={12} />}
                      撤销本次分类
                    </button>
                  </div>
                  {pending.length > 0 && (
                    <div className="rounded-lg border border-amber-500/30 bg-amber-500/10 p-3 text-[12px] text-text-secondary/80">
                      还有 {pending.length} 篇待审没有写入。回到草案确认后再应用一次即可（已写入的部分不会重复）。
                    </div>
                  )}
                  <div className="rounded-lg border border-surface-hover p-3.5 space-y-2">
                    <p className="text-[12px] text-text-primary">让新文献自动归类</p>
                    <p className="text-[11px] text-text-secondary/60">
                      把这套设置存成规则：以后新导入的文献按计划自动分类，只写入门槛以上的判定，其余进待审。
                    </p>
                    <button
                      onClick={() =>
                        useAiClassifyStore.getState().openRules({
                          dimension: run?.dimension ?? '',
                          target: run?.target ?? 'collection',
                          scopeId: scope.collectionId,
                        })
                      }
                      className="flex items-center gap-1.5 px-3 py-1.5 rounded-lg border border-surface-hover text-[12px] text-text-secondary hover:bg-surface-hover"
                    >
                      <BookMarked size={12} />
                      存为自动规则
                    </button>
                  </div>
                </>
              ) : (
                <div className="rounded-lg border border-surface-hover p-4 text-[12px] text-text-secondary">
                  本轮已应用，可关闭对话框。
                </div>
              )}

              {run && !undoSummary && (
                <div className="rounded-lg border border-surface-hover p-3.5 space-y-2">
                  <p className="text-[12px] text-text-primary">接着跑另一个维度</p>
                  <p className="text-[11px] text-text-secondary/60">
                    同一批文献可以按不同维度各分一次；每个维度独立成一套类目与词表，互不干扰。
                  </p>
                  <div className="flex flex-wrap gap-1.5">
                    {DIMENSIONS.filter((d) => d !== run.dimension).map((d) => (
                      <button
                        key={d}
                        onClick={() => void continueWith(d)}
                        className="flex items-center gap-1 px-2.5 py-1 rounded-lg border border-surface-hover text-[11px] text-text-secondary hover:bg-surface-hover hover:text-text-primary"
                      >
                        <Play size={10} />
                        {d}
                      </button>
                    ))}
                  </div>
                </div>
              )}

              <div className="flex items-center gap-2">
                <button
                  onClick={() => setStep('review')}
                  className="px-3 py-1.5 rounded-lg border border-surface-hover text-[12px] text-text-secondary hover:bg-surface-hover"
                >
                  返回草案
                </button>
                <button
                  onClick={close}
                  className="px-3 py-1.5 rounded-lg bg-primary text-black text-[12px] font-medium hover:bg-primary/90"
                >
                  完成
                </button>
              </div>
            </div>
          </div>
        )}
      </div>
    </div>
  );
}

/** Name field that reports the live value (local tree edit) and, on blur, the
 *  committed value together with the name it had on focus — so the caller can
 *  remap the assignments that referenced the old path. */
function GroupNameInput({
  value,
  placeholder,
  compact,
  onLocalChange,
  onCommit,
  onRemove,
}: {
  value: string;
  placeholder: string;
  compact?: boolean;
  onLocalChange: (next: string) => void;
  onCommit: (oldValue: string, next: string) => void;
  onRemove: () => void;
}) {
  const startRef = useRef<string | null>(null);
  return (
    <div className="flex items-center gap-1.5">
      <input
        value={value}
        onChange={(e) => onLocalChange(e.target.value)}
        onFocus={() => {
          startRef.current = value;
        }}
        onBlur={(e) => {
          const old = startRef.current ?? '';
          startRef.current = null;
          onCommit(old, e.target.value);
        }}
        placeholder={placeholder}
        className={`flex-1 min-w-0 bg-background border border-surface-hover rounded px-2 py-1 text-text-primary outline-none focus:border-primary/50 ${
          compact ? 'text-[11px]' : 'text-[12px]'
        }`}
      />
      <button
        onClick={onRemove}
        className="p-1 rounded text-text-secondary/60 hover:text-red-400 hover:bg-surface-hover"
        title="删除该类目"
      >
        <Trash2 size={12} />
      </button>
    </div>
  );
}

function PathCounts({ counts }: { counts: { accepted: number; pending: number } }) {
  return (
    <>
      {counts.accepted > 0 && <span className="text-emerald-400">{counts.accepted} 篇</span>}
      {counts.pending > 0 && <span className="text-amber-400">{counts.pending} 待审</span>}
      {counts.accepted === 0 && counts.pending === 0 && (
        <span className="text-text-secondary/40">0 篇</span>
      )}
    </>
  );
}

function StatusBadge({ item }: { item: ClassifyItem }) {
  if (item.status === 'failed') {
    return <span className="text-[10px] text-red-400">失败</span>;
  }
  if (item.labels.length === 0) {
    return <span className="text-[10px] text-text-secondary/50">待定</span>;
  }
  if (item.status === 'pending' && !item.edited) {
    return <span className="text-[10px] text-amber-400">待审</span>;
  }
  return <span className="text-[10px] text-emerald-400">已接受</span>;
}

/** Multi-label editor for the tag target: chips + an "add" select. */
function LabelChips({
  labels,
  paths,
  onAdd,
  onRemove,
}: {
  labels: string[];
  paths: string[];
  onAdd: (label: string) => void;
  onRemove: (label: string) => void;
}) {
  const available = paths.filter((p) => !labels.includes(p));
  const canAdd = labels.length < MAX_LABELS && available.length > 0;
  return (
    <div className="space-y-1">
      <div className="flex flex-wrap gap-1">
        {labels.length === 0 && <span className="text-[11px] text-amber-400">待定</span>}
        {labels.map((l) => (
          <span
            key={l}
            className="inline-flex items-center gap-0.5 px-1.5 py-0.5 rounded bg-primary/15 text-primary text-[10px]"
          >
            <span className="max-w-[110px] truncate" title={l}>
              {l}
            </span>
            <button onClick={() => onRemove(l)} className="hover:text-red-400" title="移除该标签">
              <X size={9} />
            </button>
          </span>
        ))}
      </div>
      {canAdd && (
        <select
          value=""
          onChange={(e) => {
            if (e.target.value) onAdd(e.target.value);
          }}
          className="w-full bg-background border border-surface-hover rounded px-1 py-0.5 text-[10px] text-text-secondary outline-none focus:border-primary/50"
        >
          <option value="">+ 加标签</option>
          {available.map((p) => (
            <option key={p} value={p}>
              {p}
            </option>
          ))}
        </select>
      )}
    </div>
  );
}

function remainingHint(papers: number, sampleFirst: boolean, sampleSize: number) {
  if (!sampleFirst) return '';
  const rest = papers - Math.min(sampleSize, papers);
  return rest > 0 ? `（剩余 ${rest} 篇确认后再判定）` : '';
}
