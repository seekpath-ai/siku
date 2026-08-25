import { useEffect } from 'react';
import { X } from 'lucide-react';
import ReactMarkdown from 'react-markdown';
import remarkGfm from 'remark-gfm';
import remarkMath from 'remark-math';
import rehypeKatex from 'rehype-katex';
import { MarkdownCode, MarkdownPre } from '@/components/chat/CodeBlock';
import { HELP_MARKDOWN } from './helpContent';

interface Props {
  onClose: () => void;
}

/** Built-in help dialog for the notes page. */
export function HelpDialog({ onClose }: Props) {
  useEffect(() => {
    const onDown = (e: KeyboardEvent) => {
      if (e.key === 'Escape') onClose();
    };
    window.addEventListener('keydown', onDown);
    return () => window.removeEventListener('keydown', onDown);
  }, [onClose]);

  return (
    <div className="fixed inset-0 z-[200] flex items-center justify-center">
      <div className="absolute inset-0 bg-black/50 backdrop-blur-sm" onClick={onClose} />
      <div className="relative w-[640px] max-w-[92vw] h-[560px] max-h-[84vh] flex flex-col bg-surface border border-surface-hover rounded-xl shadow-2xl overflow-hidden">
        <div className="flex items-center justify-between px-4 py-2.5 border-b border-surface-hover shrink-0">
          <span className="text-sm font-medium text-text-primary">帮助</span>
          <button
            onClick={onClose}
            className="p-1 rounded text-text-secondary/60 hover:text-text-primary hover:bg-surface-hover transition-colors"
            aria-label="关闭帮助"
          >
            <X size={14} />
          </button>
        </div>
        <div className="flex-1 overflow-y-auto p-6 prose prose-sm prose-invert max-w-none">
          <ReactMarkdown
            remarkPlugins={[remarkGfm, remarkMath]}
            rehypePlugins={[[rehypeKatex, { throwOnError: false }]]}
            components={{
              code: MarkdownCode,
              pre: MarkdownPre,
            }}
          >
            {HELP_MARKDOWN}
          </ReactMarkdown>
        </div>
      </div>
    </div>
  );
}
