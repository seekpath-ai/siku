import type { LlmProviderType } from '@/lib/types';

interface Props {
  value: LlmProviderType;
  onChange: (p: LlmProviderType) => void;
}

const providers: { value: LlmProviderType; label: string }[] = [
  { value: 'deepseek', label: 'DeepSeek' },
  { value: 'openai', label: 'OpenAI' },
  { value: 'anthropic', label: 'Claude' },
  { value: 'siliconflow', label: 'SiliconFlow' },
  { value: 'ollama', label: 'Ollama' },
];

export function ProviderSelector({ value, onChange }: Props) {
  return (
    <select
      value={value}
      onChange={(e) => onChange(e.target.value as LlmProviderType)}
      className="bg-surface border border-surface-hover rounded-lg px-3 py-2 text-sm text-text-primary focus:outline-none focus:border-primary"
    >
      {providers.map((p) => (
        <option key={p.value} value={p.value}>
          {p.label}
        </option>
      ))}
    </select>
  );
}
