import { useMemo } from 'react';
import ReactMarkdown from 'react-markdown';
import { normalizeMathDelimiters } from '@/lib/mathDelimiters';
import {
  assistantMarkdownComponents,
  assistantRehypePlugins,
  assistantRemarkPlugins,
  markdownUrlTransform,
} from './markdownConfig';

interface Props {
  content: string;
}

export function StreamingContent({ content }: Props) {
  const normalized = useMemo(() => normalizeMathDelimiters(content), [content]);
  return (
    <div className="prose prose-sm prose-invert max-w-none [&>*:first-child]:mt-0 [overflow-wrap:anywhere]">
      <ReactMarkdown
        remarkPlugins={assistantRemarkPlugins}
        rehypePlugins={assistantRehypePlugins}
        urlTransform={markdownUrlTransform}
        components={assistantMarkdownComponents}
      >
        {normalized}
      </ReactMarkdown>
      <span className="inline-block w-1.5 h-4 bg-codex-accent animate-pulse ml-0.5 align-middle rounded-sm" />
    </div>
  );
}
