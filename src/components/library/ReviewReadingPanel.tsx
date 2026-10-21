import { AlertTriangle, Check, ChevronDown, ChevronRight, Loader2, RotateCcw, Save } from 'lucide-react';
import type { ReviewPaperCard, ReviewSource } from '@/lib/tauri';
import { parseJsonArray } from '@/lib/types';

export interface ReviewCardDraft {
  card: ReviewPaperCard;
  dirty: boolean;
}

interface ReviewReadingPanelProps {
  sources: ReviewSource[];
  drafts: Record<string, ReviewCardDraft>;
  expandedPaperId: string | null;
  retryingPaperId: string | null;
  savingPaperId: string | null;
  donePapers: number;
  totalPapers: number;
  synthesisReady: boolean;
  synthesizing?: boolean;
  progressMessage?: string | null;
  onToggle: (paperId: string) => void;
  onCardChange: (paperId: string, card: ReviewPaperCard) => void;
  onSave: (paperId: string) => void;
  onRetry: (paperId: string) => void;
}

const SOURCE_KIND_LABELS = {
  fulltext: '全文',
  abstract: '仅摘要',
  unusable: '不可用',
} as const;

const SOURCE_STATUS_LABELS = {
  pending: '等待',
  running: '阅读中',
  done: '完成',
  failed: '失败',
} as const;

function formatAuthors(authors: string): string {
  if (!authors || authors === '[]') return '';
  const parsed = parseJsonArray(authors);
  return parsed.length > 0 ? parsed.join('、') : authors;
}

function CardTextField({
  label,
  value,
  onChange,
  rows = 2,
}: {
  label: string;
  value: string;
  onChange: (value: string) => void;
  rows?: number;
}) {
  return (
    <label className="block space-y-1">
      <span className="text-[11px] text-text-secondary/70">{label}</span>
      <textarea
        value={value}
        onChange={(e) => onChange(e.target.value)}
        rows={rows}
        className="w-full bg-background border border-surface-hover rounded px-2 py-1.5 text-[12px] leading-relaxed text-text-primary outline-none focus:border-primary/50 resize-y"
      />
    </label>
  );
}

export function ReviewReadingPanel({
  sources,
  drafts,
  expandedPaperId,
  retryingPaperId,
  savingPaperId,
  donePapers,
  totalPapers,
  synthesisReady,
  synthesizing = false,
  progressMessage,
  onToggle,
  onCardChange,
  onSave,
  onRetry,
}: ReviewReadingPanelProps) {
  const successCount = sources.filter((s) => s.status === 'done' && s.card).length;
  const failedCount = sources.filter((s) => s.status === 'failed').length;
  const unusableCount = sources.filter((s) => s.sourceKind === 'unusable').length;
  const settled = sources.length > 0 && sources.every((s) => s.status === 'done' || s.status === 'failed');
  const progressPercent = totalPapers > 0 ? Math.min(100, Math.round((donePapers / totalPapers) * 100)) : 0;

  if (sources.length === 0) {
    return (
      <div className="flex-1 min-h-0 flex flex-col items-center justify-center gap-2 text-center border border-dashed border-surface-hover rounded-lg">
        <p className="text-[13px] text-text-secondary">尚未开始文献阅读</p>
        <p className="text-[11px] text-text-secondary/50">开始后会逐篇显示全文、摘要降级和证据卡状态。</p>
      </div>
    );
  }

  return (
    <div className="flex-1 min-h-0 overflow-y-auto pr-0.5 space-y-2.5">
      <div className="rounded-lg border border-surface-hover/70 p-3 space-y-2">
        <div className="flex items-center gap-2 text-[12px] text-text-secondary">
          <span>阅读进度 {donePapers}/{totalPapers}</span>
          <span className="text-emerald-400/80">可用证据卡 {successCount}</span>
          {failedCount > 0 && <span className="text-red-400">失败 {failedCount}</span>}
          {unusableCount > 0 && <span className="text-amber-400/80">不可用 {unusableCount}</span>}
        </div>
        <div className="h-1.5 rounded-full bg-background overflow-hidden">
          <div className="h-full bg-primary/70 transition-all" style={{ width: `${progressPercent}%` }} />
        </div>
        {!settled && <p className="text-[11px] text-text-secondary/55">{progressMessage || '正在预检来源并提取证据卡…'}</p>}
        {settled && synthesizing && !synthesisReady && (
          <p className="flex items-center gap-1.5 text-[11px] text-primary/90">
            <Loader2 size={11} className="animate-spin" />
            证据卡已完成，正在生成跨文献综合，完成后即可进入大纲…
          </p>
        )}
        {settled && !synthesizing && successCount > 0 && !synthesisReady && (
          <p className="text-[11px] text-amber-400">证据卡已保留，但跨文献综合尚未完成；请继续生成综合后再进入大纲。</p>
        )}
        {settled && synthesisReady && failedCount === 0 && successCount > 0 && (
          <p className="text-[11px] text-emerald-400/80">阅读与综合已结束，可检查证据卡后进入大纲。</p>
        )}
        {settled && failedCount > 0 && (
          <p className="text-[11px] text-amber-400">部分文献阅读失败；可单独重试，或基于已成功文献继续。</p>
        )}
        {settled && successCount === 0 && (
          <p className="text-[11px] text-red-400">当前没有可用证据卡，需重试失败文献或更换文献集合。</p>
        )}
      </div>

      <ul className="space-y-2">
        {sources.map((source) => {
          const expanded = expandedPaperId === source.paperId;
          const draft = drafts[source.paperId];
          const card = draft?.card ?? source.card;
          const retrying = retryingPaperId === source.paperId;
          const saving = savingPaperId === source.paperId;
          return (
            <li key={source.paperId} className="rounded-lg border border-surface-hover/70 overflow-hidden">
              <div className="flex items-stretch bg-background/20">
                <button
                  type="button"
                  onClick={() => onToggle(source.paperId)}
                  className="flex-1 min-w-0 flex items-start gap-2 text-left px-3 py-2.5 hover:bg-surface-hover/40"
                >
                  {expanded ? (
                    <ChevronDown size={14} className="mt-0.5 shrink-0 text-text-secondary/60" />
                  ) : (
                    <ChevronRight size={14} className="mt-0.5 shrink-0 text-text-secondary/60" />
                  )}
                  <span className="text-[11px] text-text-secondary/45 mt-0.5 shrink-0">#{source.paperIndex}</span>
                  <span className="flex-1 min-w-0">
                    <span className="flex items-center gap-1.5 flex-wrap">
                      <span className="text-[13px] text-text-primary break-words">{source.title || '未命名文献'}</span>
                      <span
                        className={`px-1.5 py-0.5 rounded text-[10px] ${
                          source.sourceKind === 'fulltext'
                            ? 'bg-primary/10 text-primary'
                            : source.sourceKind === 'abstract'
                              ? 'bg-amber-400/10 text-amber-300'
                              : 'bg-red-400/10 text-red-300'
                        }`}
                      >
                        {SOURCE_KIND_LABELS[source.sourceKind]}
                      </span>
                    </span>
                    <span className="block mt-1 text-[10px] text-text-secondary/50 truncate">
                      {[formatAuthors(source.authors), source.year].filter(Boolean).join(' · ') || '无作者与年份信息'}
                    </span>
                    <span className="block mt-1 text-[10px] text-text-secondary/45">
                      全文片段 {source.chunkCount} · 摘要 {source.abstractChars} 字
                    </span>
                  </span>
                  <span className="flex items-center gap-1 shrink-0 mt-0.5 text-[11px]">
                    {source.status === 'running' && <Loader2 size={12} className="animate-spin text-primary" />}
                    {source.status === 'done' && <Check size={12} className="text-emerald-400" />}
                    {source.status === 'failed' && <AlertTriangle size={12} className="text-red-400" />}
                    <span className={source.status === 'failed' ? 'text-red-400' : 'text-text-secondary/60'}>
                      {SOURCE_STATUS_LABELS[source.status]}
                    </span>
                  </span>
                </button>
                {source.status === 'failed' && (
                  <button
                    type="button"
                    onClick={() => onRetry(source.paperId)}
                    disabled={retryingPaperId !== null}
                    className="flex items-center gap-1 px-3 text-[11px] text-text-secondary hover:bg-surface-hover hover:text-text-primary disabled:opacity-40 shrink-0"
                  >
                    {retrying ? <Loader2 size={11} className="animate-spin" /> : <RotateCcw size={11} />}
                    重试
                  </button>
                )}
              </div>

              {source.error && <p className="px-3 py-2 border-t border-surface-hover/50 text-[11px] text-red-400">{source.error}</p>}

              {expanded && (
                <div className="border-t border-surface-hover/60 p-3 space-y-3">
                  {!card ? (
                    <div className="py-4 text-center text-[12px] text-text-secondary/55">
                      {source.sourceKind === 'unusable' ? '该文献没有可用于提取证据卡的全文或摘要。' : '尚未生成证据卡。'}
                    </div>
                  ) : (
                    <>
                      <div className="flex items-center gap-2">
                        <span className="text-[11px] text-text-secondary/55">
                          {card.sourceType === 'fulltext' ? '基于全文的证据卡' : '基于摘要的证据卡'}
                        </span>
                        {card.edited && <span className="text-[10px] text-primary/80">已编辑</span>}
                        {draft?.dirty && <span className="text-[10px] text-amber-400">有未保存修改</span>}
                        <button
                          type="button"
                          onClick={() => onSave(source.paperId)}
                          disabled={!draft?.dirty || savingPaperId !== null}
                          className="ml-auto flex items-center gap-1 px-2 py-1 rounded border border-surface-hover text-[11px] text-text-secondary hover:bg-surface-hover hover:text-text-primary disabled:opacity-40"
                        >
                          {saving ? <Loader2 size={11} className="animate-spin" /> : <Save size={11} />}
                          保存卡片
                        </button>
                      </div>

                      <CardTextField
                        label="研究问题"
                        value={card.researchQuestion}
                        onChange={(researchQuestion) => onCardChange(source.paperId, { ...card, researchQuestion, edited: true })}
                      />
                      <CardTextField
                        label="方法"
                        value={card.methods}
                        onChange={(methods) => onCardChange(source.paperId, { ...card, methods, edited: true })}
                      />
                      <CardTextField
                        label="数据与样本"
                        value={card.dataAndSample}
                        onChange={(dataAndSample) => onCardChange(source.paperId, { ...card, dataAndSample, edited: true })}
                      />

                      <div className="space-y-1.5">
                        <span className="text-[11px] text-text-secondary/70">主要发现</span>
                        {card.findings.length === 0 && (
                          <p className="rounded border border-dashed border-surface-hover px-2 py-2 text-[11px] text-text-secondary/45">
                            未提取到结构化发现。
                          </p>
                        )}
                        {card.findings.map((finding, index) => (
                          <div key={index} className="rounded border border-surface-hover/60 p-2 space-y-1">
                            <textarea
                              value={finding.claim}
                              onChange={(e) =>
                                onCardChange(source.paperId, {
                                  ...card,
                                  findings: card.findings.map((item, i) =>
                                    i === index ? { ...item, claim: e.target.value } : item
                                  ),
                                  edited: true,
                                })
                              }
                              rows={2}
                              className="w-full bg-background border border-surface-hover rounded px-2 py-1.5 text-[12px] text-text-primary outline-none focus:border-primary/50 resize-y"
                            />
                            <p className="text-[10px] text-text-secondary/45">
                              关联证据：{finding.evidenceIds.length > 0 ? finding.evidenceIds.join('、') : '无'}
                            </p>
                          </div>
                        ))}
                      </div>

                      <CardTextField
                        label="局限"
                        value={card.limitations}
                        onChange={(limitations) => onCardChange(source.paperId, { ...card, limitations, edited: true })}
                      />
                      <CardTextField
                        label="与研究问题的相关性"
                        value={card.relevance}
                        onChange={(relevance) => onCardChange(source.paperId, { ...card, relevance, edited: true })}
                      />

                      <div className="space-y-1.5">
                        <span className="text-[11px] text-text-secondary/70">原文证据（只读）</span>
                        {card.evidence.length === 0 && (
                          <p className="rounded border border-dashed border-surface-hover px-2 py-2 text-[11px] text-amber-400/80">
                            该卡片没有通过校验的原文证据。
                          </p>
                        )}
                        {card.evidence.map((item) => (
                          <div key={item.id} className="rounded bg-background/60 border border-surface-hover/50 p-2 space-y-1.5">
                            <div className="flex items-center gap-2 flex-wrap text-[10px] text-text-secondary/45">
                              <span className="text-primary/70">{item.id}</span>
                              <span>{item.page != null ? `第 ${item.page} 页` : '无页码'}</span>
                              {item.chunkId && <span>chunk: {item.chunkId}</span>}
                            </div>
                            <p className="text-[11px] leading-relaxed text-text-secondary/85 whitespace-pre-wrap break-words">
                              {item.quote || item.content || '（空证据）'}
                            </p>
                            {item.quote && item.content && item.content !== item.quote && (
                              <p className="text-[10px] leading-relaxed text-text-secondary/45 whitespace-pre-wrap break-words">
                                上下文：{item.content}
                              </p>
                            )}
                          </div>
                        ))}
                      </div>
                    </>
                  )}
                </div>
              )}
            </li>
          );
        })}
      </ul>
    </div>
  );
}
