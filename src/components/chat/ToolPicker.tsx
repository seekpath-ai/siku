import { useEffect, useRef } from 'react';
import { TOOL_CATEGORIES, type AgentToolCategory } from '@/lib/agent-tools';

interface ToolPickerProps {
  value: string[];
  onChange: (tools: string[]) => void;
}

/** Category checkbox with "部分选中" (indeterminate) state. */
function CategoryCheckbox({
  checked,
  indeterminate,
  onChange,
  label,
}: {
  checked: boolean;
  indeterminate: boolean;
  onChange: () => void;
  label: string;
}) {
  const ref = useRef<HTMLInputElement>(null);
  useEffect(() => {
    if (ref.current) ref.current.indeterminate = indeterminate && !checked;
  }, [indeterminate, checked]);
  return (
    <label className="flex items-center gap-2 text-xs font-medium text-codex-secondary cursor-pointer select-none">
      <input
        ref={ref}
        type="checkbox"
        checked={checked}
        onChange={onChange}
        className="rounded border-codex-border bg-codex-bg text-codex-accent"
      />
      {label}
    </label>
  );
}

/** Grouped tool picker: toggle individual tools or an entire category. */
export function ToolPicker({ value, onChange }: ToolPickerProps) {
  const toggleTool = (key: string) => {
    onChange(value.includes(key) ? value.filter((t) => t !== key) : [...value, key]);
  };

  const toggleCategory = (cat: AgentToolCategory) => {
    const keys = cat.tools.map((t) => t.key);
    const allSelected = keys.every((k) => value.includes(k));
    if (allSelected) {
      onChange(value.filter((t) => !keys.includes(t)));
    } else {
      onChange([...new Set([...value, ...keys])]);
    }
  };

  return (
    <div className="space-y-2 max-h-56 overflow-y-auto pr-1">
      {TOOL_CATEGORIES.map((cat) => {
        const keys = cat.tools.map((t) => t.key);
        const allSelected = keys.every((k) => value.includes(k));
        const someSelected = keys.some((k) => value.includes(k));
        return (
          <div key={cat.name}>
            <CategoryCheckbox
              checked={allSelected}
              indeterminate={someSelected}
              onChange={() => toggleCategory(cat)}
              label={cat.name}
            />
            <div className="grid grid-cols-2 gap-x-2 pl-5 mt-0.5">
              {cat.tools.map((t) => (
                <label
                  key={t.key}
                  className="flex items-center gap-1.5 text-xs text-codex-secondary cursor-pointer select-none"
                >
                  <input
                    type="checkbox"
                    checked={value.includes(t.key)}
                    onChange={() => toggleTool(t.key)}
                    className="rounded border-codex-border bg-codex-bg text-codex-accent shrink-0"
                  />
                  <span className="truncate" title={t.label}>
                    {t.label}
                  </span>
                </label>
              ))}
            </div>
          </div>
        );
      })}
    </div>
  );
}
