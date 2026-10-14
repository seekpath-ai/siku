import { useState } from 'react';
import { Plus, X, Sparkles, Loader2 } from 'lucide-react';
import { researchGenerateTopic } from '@/lib/tauri';

interface Props {
  /** Prefill for edit mode; omitted = create mode. */
  initial?: { name: string; description?: string; keywords: string[] };
  submitLabel?: string;
  onSubmit: (name: string, keywords: string[], description?: string) => Promise<void>;
  onCancel: () => void;
}

/** Topic create/edit form. The AI-assist box turns a natural-language
 * research interest into a proposed name + English keywords + description,
 * prefilled here for the user to edit before confirming (single LLM call,
 * nothing is persisted until submit). */
export function TopicForm({ initial, submitLabel = '创建', onSubmit, onCancel }: Props) {
  const [name, setName] = useState(initial?.name ?? '');
  const [description, setDescription] = useState(initial?.description ?? '');
  const [keywords, setKeywords] = useState<string[]>(initial?.keywords ?? []);
  const [kwInput, setKwInput] = useState('');
  const [blurb, setBlurb] = useState('');
  const [generating, setGenerating] = useState(false);
  const [genError, setGenError] = useState<string | null>(null);

  const handleAddKeyword = () => {
    const kw = kwInput.trim();
    if (kw && !keywords.includes(kw)) {
      setKeywords([...keywords, kw]);
      setKwInput('');
    }
  };

  const handleGenerate = async () => {
    if (!blurb.trim() || generating) return;
    setGenerating(true);
    setGenError(null);
    try {
      const draft = await researchGenerateTopic(blurb.trim());
      setName(draft.name);
      setKeywords(draft.keywords);
      setDescription(draft.description);
    } catch (err) {
      setGenError(String(err instanceof Error ? err.message : err));
    } finally {
      setGenerating(false);
    }
  };

  const handleSubmit = async () => {
    if (!name.trim()) return;
    await onSubmit(name.trim(), keywords, description.trim() || undefined);
  };

  return (
    <div className="p-4 bg-surface border border-surface-hover rounded-xl space-y-3">
      {/* AI 辅助生成：自然语言 → 预填表单 */}
      <div className="flex gap-2">
        <input
          type="text" value={blurb} onChange={(e) => setBlurb(e.target.value)}
          onKeyDown={(e) => e.key === 'Enter' && (e.preventDefault(), handleGenerate())}
          placeholder="用一句话描述研究方向，AI 帮你生成课题…"
          className="flex-1 bg-background border border-surface-hover rounded-lg px-3 py-2 text-sm text-text-primary focus:outline-none focus:border-primary"
        />
        <button
          onClick={handleGenerate}
          disabled={!blurb.trim() || generating}
          className="flex items-center gap-1 px-3 py-2 rounded-lg bg-primary/10 text-primary text-sm hover:bg-primary/20 disabled:opacity-50"
          title="由 AI 生成课题名称、英文关键词和描述，可再修改"
        >
          {generating ? <Loader2 size={14} className="animate-spin" /> : <Sparkles size={14} />}
          AI 生成
        </button>
      </div>
      {genError && <p className="text-xs text-red-400">{genError}</p>}

      <input
        type="text" value={name} onChange={(e) => setName(e.target.value)}
        placeholder="课题名称"
        className="w-full bg-background border border-surface-hover rounded-lg px-3 py-2 text-sm text-text-primary focus:outline-none focus:border-primary"
      />
      <textarea
        value={description} onChange={(e) => setDescription(e.target.value)}
        placeholder="描述（可选）" rows={2}
        className="w-full bg-background border border-surface-hover rounded-lg px-3 py-2 text-sm text-text-primary focus:outline-none focus:border-primary resize-none"
      />
      <div className="flex gap-2">
        <input
          type="text" value={kwInput} onChange={(e) => setKwInput(e.target.value)}
          onKeyDown={(e) => e.key === 'Enter' && (e.preventDefault(), handleAddKeyword())}
          placeholder="添加关键词（英文检索词效果更好）"
          className="flex-1 bg-background border border-surface-hover rounded-lg px-3 py-2 text-sm text-text-primary focus:outline-none focus:border-primary"
        />
        <button onClick={handleAddKeyword} className="px-3 py-2 rounded-lg bg-surface-hover text-text-primary text-sm"><Plus size={16} /></button>
      </div>
      {keywords.length > 0 && (
        <div className="flex flex-wrap gap-1">
          {keywords.map((kw) => (
            <span key={kw} className="flex items-center gap-1 text-xs px-2 py-1 rounded bg-primary/10 text-primary">
              {kw}
              <button onClick={() => setKeywords(keywords.filter((k) => k !== kw))}><X size={10} /></button>
            </span>
          ))}
        </div>
      )}
      <div className="flex gap-2">
        <button onClick={handleSubmit} className="px-4 py-2 rounded-lg bg-primary text-white text-sm">{submitLabel}</button>
        <button onClick={onCancel} className="px-4 py-2 rounded-lg bg-surface-hover text-text-secondary text-sm">取消</button>
      </div>
    </div>
  );
}
