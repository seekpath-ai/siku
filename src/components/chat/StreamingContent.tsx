import { useMemo } from 'react';
import ReactMarkdown from 'react-markdown';
import remarkGfm from 'remark-gfm';
import remarkMath from 'remark-math';
import rehypeKatex from 'rehype-katex';
import { MarkdownCode, MarkdownPre } from './CodeBlock';
import { ExternalLink } from '@/components/ui/ExternalLink';
import { normalizeMathDelimiters } from '@/lib/mathDelimiters';

interface Props {
  content: string;
}

export function StreamingContent({ content }: Props) {
  const normalized = useMemo(() => normalizeMathDelimiters(content), [content]);
  return (
    <div className="prose prose-sm prose-invert max-w-none [&>*:first-child]:mt-0 [overflow-wrap:anywhere]">
      <ReactMarkdown
        remarkPlugins={[remarkGfm, remarkMath]}
        rehypePlugins={[[rehypeKatex, { throwOnError: false }]]}
        components={{
          a: ExternalLink,
          code: MarkdownCode,
          pre: MarkdownPre,
        }}
      >
        {normalized}
      </ReactMarkdown>
      <span className="inline-block w-1.5 h-4 bg-codex-accent animate-pulse ml-0.5 align-middle rounded-sm" />
    </div>
  );
}
