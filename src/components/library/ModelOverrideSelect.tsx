import { useEffect, useState } from 'react';
import { llmProviderList } from '@/lib/tauri';
import type { LlmProvider } from '@/lib/types';

/** Optional model override for the batch AI features (AI 表格列 / AI 综述):
 *  default = follow the starred provider, or pin any configured one. */
export function ModelOverrideSelect({
  value,
  onChange,
  disabled,
  className = '',
}: {
  /** '' = follow the default (starred) provider. */
  value: string;
  onChange: (id: string) => void;
  disabled?: boolean;
  className?: string;
}) {
  const [providers, setProviders] = useState<LlmProvider[]>([]);
  useEffect(() => {
    llmProviderList().then(setProviders).catch(() => {});
  }, []);
  const starred = providers.find((p) => p.is_default);

  return (
    <select
      value={value}
      onChange={(e) => onChange(e.target.value)}
      disabled={disabled}
      title="选择本次任务使用的模型，默认跟随设置中的星标模型"
      className={`bg-background border border-surface-hover rounded-lg px-2 py-1.5 text-[12px] text-text-primary outline-none focus:border-primary/50 disabled:opacity-50 ${className}`}
    >
      <option value="">跟随默认{starred ? `（${starred.name}）` : ''}</option>
      {providers.map((p) => (
        <option key={p.id} value={p.id}>
          {p.name}（{p.model}）
        </option>
      ))}
    </select>
  );
}
