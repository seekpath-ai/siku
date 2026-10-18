import { ArrowDown, ArrowUp, Plus, Trash2 } from 'lucide-react';
import type { ReviewSection, ReviewSource, ReviewSynthesis, ReviewSynthesisItem } from '@/lib/tauri';

interface ReviewOutlineEditorProps {
  outline: ReviewSection[];
  synthesis: ReviewSynthesis | null;
  sources: ReviewSource[];
  onChange: (outline: ReviewSection[]) => void;
}

function paperReferences(paperIds: string[], sources: ReviewSource[]): string {
  if (paperIds.length === 0) return '未指定';
  return paperIds
    .map((id) => {
      const source = sources.find((item) => item.paperId === id);
      return source ? `#${source.paperIndex}` : id;
    })
    .join('、');
}

function SynthesisGroup({
  title,
  items,
  sources,
  emptyText,
}: {
  title: string;
  items: ReviewSynthesisItem[];
  sources: ReviewSource[];
  emptyText: string;
}) {
  return (
    <section className="rounded-lg border border-surface-hover/60 p-2.5 space-y-2">
      <h3 className="text-[12px] font-medium text-text-primary">{title}</h3>
      {items.length === 0 && <p className="text-[11px] text-text-secondary/45">{emptyText}</p>}
      {items.map((item, index) => (
        <div key={index} className="rounded bg-background/50 px-2 py-1.5 space-y-1">
          <p className="text-[12px] text-text-primary">{item.title || '未命名条目'}</p>
          <p className="text-[11px] leading-relaxed text-text-secondary/75 whitespace-pre-wrap">{item.detail}</p>
          <p className="text-[10px] text-text-secondary/45">关联文献：{paperReferences(item.paperIds, sources)}</p>
        </div>
      ))}
    </section>
  );
}

export function ReviewOutlineEditor({ outline, synthesis, sources, onChange }: ReviewOutlineEditorProps) {
  const moveSection = (index: number, delta: number) => {
    const target = index + delta;
    if (target < 0 || target >= outline.length) return;
    const next = [...outline];
    [next[index], next[target]] = [next[target], next[index]];
    onChange(next);
  };

  return (
    <div className="flex-1 min-h-0 overflow-y-auto space-y-3 pr-0.5">
      <div className="space-y-2">
        <div>
          <h2 className="text-[12px] font-medium text-text-primary">跨文献综合</h2>
          <p className="text-[11px] text-text-secondary/50">主题、共识、冲突与缺口用于约束大纲和后续写作。</p>
        </div>
        {!synthesis ? (
          <div className="rounded-lg border border-dashed border-surface-hover px-3 py-4 text-center text-[11px] text-text-secondary/50">
            暂无综合结果。生成大纲后，此处会显示后端返回的跨文献综合。
          </div>
        ) : (
          <div className="grid md:grid-cols-2 gap-2">
            <SynthesisGroup title="主题" items={synthesis.themes} sources={sources} emptyText="暂无主题归纳。" />
            <SynthesisGroup title="共识" items={synthesis.agreements} sources={sources} emptyText="暂无可确认的共识。" />
            <SynthesisGroup title="冲突" items={synthesis.conflicts} sources={sources} emptyText="暂无可确认的冲突。" />
            <SynthesisGroup title="研究缺口" items={synthesis.gaps} sources={sources} emptyText="暂无研究缺口归纳。" />
          </div>
        )}
      </div>

      <div className="space-y-2.5">
        <div>
          <h2 className="text-[12px] font-medium text-text-primary">综述大纲</h2>
          <p className="text-[11px] text-text-secondary/50">
            可改标题与要点、增删章节、调整顺序；每节关联的 paperIds 会原样保留。
          </p>
        </div>
        {outline.length === 0 && (
          <div className="rounded-lg border border-dashed border-surface-hover px-3 py-6 text-center space-y-1">
            <p className="text-[12px] text-text-secondary">尚未生成大纲</p>
            <p className="text-[11px] text-text-secondary/45">点击下方“生成大纲”，或手动添加章节。</p>
          </div>
        )}
        {outline.map((section, sectionIndex) => (
          <div key={sectionIndex} className="rounded-lg border border-surface-hover/70 p-2.5 space-y-1.5">
            <div className="flex items-center gap-1.5">
              <span className="text-[11px] text-text-secondary/50 w-4">{sectionIndex + 1}.</span>
              <input
                value={section.title}
                onChange={(e) =>
                  onChange(outline.map((item, index) => (index === sectionIndex ? { ...item, title: e.target.value } : item)))
                }
                placeholder="章节标题"
                className="flex-1 min-w-0 bg-background border border-surface-hover rounded px-2 py-1 text-[12px] text-text-primary outline-none focus:border-primary/50"
              />
              <button
                type="button"
                onClick={() => moveSection(sectionIndex, -1)}
                disabled={sectionIndex === 0}
                className="p-1 rounded text-text-secondary/60 hover:bg-surface-hover disabled:opacity-30"
                title="上移"
              >
                <ArrowUp size={12} />
              </button>
              <button
                type="button"
                onClick={() => moveSection(sectionIndex, 1)}
                disabled={sectionIndex === outline.length - 1}
                className="p-1 rounded text-text-secondary/60 hover:bg-surface-hover disabled:opacity-30"
                title="下移"
              >
                <ArrowDown size={12} />
              </button>
              <button
                type="button"
                onClick={() => onChange(outline.filter((_, index) => index !== sectionIndex))}
                className="p-1 rounded text-text-secondary/60 hover:text-red-400 hover:bg-surface-hover"
                title="删除该节"
              >
                <Trash2 size={12} />
              </button>
            </div>
            <p className="pl-5 text-[10px] text-text-secondary/45">
              关联文献：{paperReferences(section.paperIds, sources)}
            </p>
            <textarea
              value={section.points}
              onChange={(e) =>
                onChange(outline.map((item, index) => (index === sectionIndex ? { ...item, points: e.target.value } : item)))
              }
              rows={2}
              placeholder="本节要点"
              className="w-full bg-background border border-surface-hover rounded px-2 py-1 text-[11px] text-text-primary outline-none focus:border-primary/50 resize-none"
            />
            {section.subsections.map((subsection, subsectionIndex) => (
              <div key={subsectionIndex} className="flex items-center gap-1.5 pl-5">
                <span className="text-[11px] text-text-secondary/40">
                  {sectionIndex + 1}.{subsectionIndex + 1}
                </span>
                <input
                  value={subsection}
                  onChange={(e) =>
                    onChange(
                      outline.map((item, index) =>
                        index === sectionIndex
                          ? {
                              ...item,
                              subsections: item.subsections.map((value, i) => (i === subsectionIndex ? e.target.value : value)),
                            }
                          : item
                      )
                    )
                  }
                  placeholder="小节标题"
                  className="flex-1 min-w-0 bg-background border border-surface-hover rounded px-2 py-0.5 text-[11px] text-text-primary outline-none focus:border-primary/50"
                />
                <button
                  type="button"
                  onClick={() =>
                    onChange(
                      outline.map((item, index) =>
                        index === sectionIndex
                          ? { ...item, subsections: item.subsections.filter((_, i) => i !== subsectionIndex) }
                          : item
                      )
                    )
                  }
                  className="p-0.5 rounded text-text-secondary/50 hover:text-red-400 hover:bg-surface-hover"
                  title="删除小节"
                >
                  <Trash2 size={11} />
                </button>
              </div>
            ))}
            <button
              type="button"
              onClick={() =>
                onChange(
                  outline.map((item, index) =>
                    index === sectionIndex ? { ...item, subsections: [...item.subsections, ''] } : item
                  )
                )
              }
              className="ml-5 flex items-center gap-1 px-1.5 py-0.5 rounded border border-dashed border-surface-hover text-[11px] text-text-secondary/70 hover:bg-surface-hover"
            >
              <Plus size={10} />
              添加小节
            </button>
          </div>
        ))}
        <button
          type="button"
          onClick={() => onChange([...outline, { title: '', points: '', subsections: [], paperIds: [] }])}
          className="w-full flex items-center justify-center gap-1 px-2 py-1.5 rounded-lg border border-dashed border-surface-hover text-[12px] text-text-secondary hover:bg-surface-hover"
        >
          <Plus size={12} />
          添加章节
        </button>
      </div>
    </div>
  );
}
