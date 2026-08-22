import { useState } from 'react';
import { Plus, X } from 'lucide-react';

interface Props {
  onCreate: (name: string, keywords: string[], description?: string) => Promise<void>;
  onCancel: () => void;
}

export function TopicForm({ onCreate, onCancel }: Props) {
  const [name, setName] = useState('');
  const [description, setDescription] = useState('');
  const [keywords, setKeywords] = useState<string[]>([]);
  const [kwInput, setKwInput] = useState('');

  const handleAddKeyword = () => {
    const kw = kwInput.trim();
    if (kw && !keywords.includes(kw)) {
      setKeywords([...keywords, kw]);
      setKwInput('');
    }
  };

  const handleSubmit = async () => {
    if (!name.trim()) return;
    await onCreate(name, keywords, description || undefined);
  };

  return (
    <div className="p-4 bg-surface border border-surface-hover rounded-xl space-y-3">
      <input
        type="text" value={name} onChange={(e) => setName(e.target.value)}
        placeholder="课题名称" autoFocus
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
          placeholder="添加关键词"
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
        <button onClick={handleSubmit} className="px-4 py-2 rounded-lg bg-primary text-white text-sm">创建</button>
        <button onClick={onCancel} className="px-4 py-2 rounded-lg bg-surface-hover text-text-secondary text-sm">取消</button>
      </div>
    </div>
  );
}
