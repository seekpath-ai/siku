import { useState, useEffect, useRef } from 'react';
import { AlertCircle, Check, ChevronDown, ChevronRight, HelpCircle, List, MessageSquareText, X } from 'lucide-react';
import { useDialogStore } from '@/stores/dialogStore';

export function Dialog() {
  const { open, type, title, message, promptOptions, selectOptions, close } = useDialogStore();
  const [inputValue, setInputValue] = useState(promptOptions?.defaultValue ?? '');
  const [selectedValue, setSelectedValue] = useState<string | null>(null);
  const [collapsed, setCollapsed] = useState<Set<string>>(new Set());
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    setInputValue(promptOptions?.defaultValue ?? '');
    setSelectedValue(null);
    setCollapsed(new Set());
  }, [promptOptions?.defaultValue, open]);

  useEffect(() => {
    if (open && type === 'prompt') {
      setTimeout(() => inputRef.current?.focus(), 50);
    }
  }, [open, type]);

  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (!open) return;
      if (e.key === 'Escape') {
        e.preventDefault();
        close(type === 'alert' ? true : null);
      } else if (e.key === 'Enter') {
        e.preventDefault();
        if (type === 'prompt') {
          close(inputValue.trim() || null);
        } else if (type === 'confirm') {
          close(true);
        } else {
          close(true);
        }
      }
    };
    window.addEventListener('keydown', handler);
    return () => window.removeEventListener('keydown', handler);
  }, [open, type, close, inputValue]);

  if (!open || !type) return null;

  const icon =
    type === 'alert' ? (
      <AlertCircle size={20} className="text-yellow-400" />
    ) : type === 'confirm' ? (
      <HelpCircle size={20} className="text-primary" />
    ) : type === 'select' ? (
      <List size={20} className="text-primary" />
    ) : (
      <MessageSquareText size={20} className="text-primary" />
    );

  return (
    <div className="fixed inset-0 z-[200] flex items-center justify-center">
      <div
        className="absolute inset-0 bg-black/50 backdrop-blur-sm"
        onClick={() => close(type === 'alert' ? true : null)}
      />
      <div className="relative w-full max-w-sm mx-4 bg-surface border border-surface-hover rounded-xl shadow-2xl overflow-hidden">
        <div className="flex items-center gap-3 px-4 py-3 border-b border-surface-hover">
          {icon}
          <span className="text-sm font-medium text-text-primary">{title}</span>
          <button
            onClick={() => close(type === 'alert' ? true : null)}
            className="ml-auto p-1 rounded text-text-secondary/60 hover:text-text-primary hover:bg-surface-hover transition-colors"
          >
            <X size={14} />
          </button>
        </div>

        <div className="px-4 py-4">
          <p className="text-sm text-text-primary leading-relaxed whitespace-pre-wrap">{message}</p>
          {type === 'prompt' && (
            promptOptions?.multiline ? (
              <textarea
                ref={inputRef as unknown as React.RefObject<HTMLTextAreaElement>}
                value={inputValue}
                onChange={(e) => setInputValue(e.target.value)}
                placeholder={promptOptions?.placeholder}
                rows={8}
                className="mt-3 w-full rounded-lg bg-background border border-surface-hover px-3 py-2 text-sm font-mono text-text-primary placeholder:text-text-secondary/40 focus:outline-none focus:border-primary/50 resize-y"
              />
            ) : (
              <input
                ref={inputRef}
                type="text"
                value={inputValue}
                onChange={(e) => setInputValue(e.target.value)}
                placeholder={promptOptions?.placeholder}
                className="mt-3 w-full h-9 rounded-lg bg-background border border-surface-hover px-3 text-sm text-text-primary placeholder:text-text-secondary/40 focus:outline-none focus:border-primary/50"
              />
            )
          )}
          {type === 'select' && selectOptions && (
            <div className="mt-3 max-h-60 overflow-y-auto rounded-lg border border-surface-hover bg-background">
              {selectOptions.options.length === 0 ? (
                <div className="px-3 py-2 text-xs text-text-secondary/60">没有可用选项</div>
              ) : (() => {
                // Hide nodes whose parent chain is collapsed. Options are in
                // pre-order (parents before children), so a single pass works.
                const hidden = new Set<string>();
                for (const o of selectOptions.options) {
                  if (o.parent && (collapsed.has(o.parent) || hidden.has(o.parent))) {
                    hidden.add(o.value);
                  }
                }
                return selectOptions.options.map((option) => {
                  if (hidden.has(option.value)) return null;
                  const depth = option.indent ?? 0;
                  const hasChildren = !!option.expandable;
                  const isCollapsed = hasChildren && collapsed.has(option.value);
                  return (
                    <div
                      key={option.value}
                      onClick={() => setSelectedValue(option.value)}
                      className="w-full flex items-center gap-2 px-3 py-1.5 text-left text-sm text-text-primary hover:bg-surface-hover transition-colors cursor-pointer"
                      style={{ paddingLeft: 12 + depth * 16 }}
                    >
                      {hasChildren ? (
                        <button
                          onClick={(e) => {
                            e.stopPropagation();
                            setCollapsed((prev) => {
                              const next = new Set(prev);
                              if (next.has(option.value)) next.delete(option.value);
                              else next.add(option.value);
                              return next;
                            });
                          }}
                          className="p-0.5 -ml-1 shrink-0 rounded text-text-secondary/60 hover:text-text-primary hover:bg-surface-hover transition-colors"
                          title={isCollapsed ? '展开' : '折叠'}
                        >
                          {isCollapsed ? <ChevronRight size={14} /> : <ChevronDown size={14} />}
                        </button>
                      ) : (
                        <span className="w-4 shrink-0" />
                      )}
                      <span
                        className={`w-4 h-4 shrink-0 flex items-center justify-center rounded border border-surface-hover transition-colors ${
                          selectedValue === option.value ? 'border-primary' : ''
                        }`}
                      >
                        {selectedValue === option.value && <Check size={12} className="text-primary" />}
                      </span>
                      <span className={`truncate ${hasChildren ? 'font-medium' : ''}`}>{option.label}</span>
                    </div>
                  );
                });
              })()}
            </div>
          )}
        </div>

        <div className="flex items-center justify-end gap-2 px-4 py-3 border-t border-surface-hover bg-surface/50">
          {type !== 'alert' && (
            <button
              onClick={() => close(null)}
              className="px-3.5 py-1.5 rounded-lg text-xs text-text-secondary hover:text-text-primary hover:bg-surface-hover transition-colors"
            >
              取消
            </button>
          )}
          <button
            onClick={() =>
              close(type === 'prompt' ? inputValue.trim() || null : type === 'select' ? selectedValue : true)
            }
            disabled={type === 'select' && !selectedValue}
            className="px-3.5 py-1.5 rounded-lg bg-primary/10 text-primary text-xs font-medium hover:bg-primary/20 transition-colors disabled:opacity-40 disabled:cursor-not-allowed"
          >
            {type === 'alert' ? '确定' : '确定'}
          </button>
        </div>
      </div>
    </div>
  );
}
