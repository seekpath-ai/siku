import { useLayoutEffect, useRef, useState, type ReactNode } from 'react';
import { ChevronDown, ChevronUp } from 'lucide-react';

interface Props {
  children: ReactNode;
  /** 折叠时的最大可见高度（px），超过该高度才出现折叠。 */
  maxHeight?: number;
  /** 渐变遮罩的起始色类名，必须与气泡背景一致，如 'from-codex-surface'。 */
  gradientFrom: string;
  buttonClassName?: string;
}

export function CollapsibleText({ children, maxHeight = 240, gradientFrom, buttonClassName }: Props) {
  const ref = useRef<HTMLDivElement>(null);
  const [collapsible, setCollapsible] = useState(false);
  const [expanded, setExpanded] = useState(false);

  useLayoutEffect(() => {
    const el = ref.current;
    if (el && el.scrollHeight > maxHeight + 32) setCollapsible(true);
  }, [maxHeight]);

  return (
    <div>
      <div className="relative">
        <div
          ref={ref}
          style={collapsible && !expanded ? { maxHeight, overflow: 'hidden' } : undefined}
        >
          {children}
        </div>
        {collapsible && !expanded && (
          <div
            className={`pointer-events-none absolute inset-x-0 bottom-0 h-10 bg-gradient-to-t ${gradientFrom} to-transparent`}
          />
        )}
      </div>
      {collapsible && (
        <button
          type="button"
          onClick={() => setExpanded((v) => !v)}
          className={buttonClassName ?? 'mt-1 inline-flex items-center gap-0.5 text-[11px] transition-colors'}
        >
          {expanded ? (
            <>收起 <ChevronUp size={12} /></>
          ) : (
            <>展开全文 <ChevronDown size={12} /></>
          )}
        </button>
      )}
    </div>
  );
}
