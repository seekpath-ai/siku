import { useMemo } from 'react';
import ReactMarkdown from 'react-markdown';
import { normalizeMathDelimiters } from '@/lib/mathDelimiters';
import { splitArtifactSection } from '@/lib/artifactSection';
import { ArtifactSection } from './ArtifactSection';
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
  const split = useMemo(() => splitArtifactSection(content), [content]);
  const normalized = useMemo(() => normalizeMathDelimiters(split.body), [split.body]);
  return (
    <>
      <div className="prose prose-sm prose-invert max-w-none [&>*:first-child]:mt-0 [overflow-wrap:anywhere]">
        <ReactMarkdown
          remarkPlugins={assistantRemarkPlugins}
          rehypePlugins={assistantRehypePlugins}
          urlTransform={markdownUrlTransform}
          components={assistantMarkdownComponents}
        >
          {normalized}
        </ReactMarkdown>
        {split.artifacts.length === 0 && (
          <span className="inline-block w-1.5 h-4 bg-codex-accent animate-pulse ml-0.5 align-middle rounded-sm" />
        )}
      </div>
      <ArtifactSection artifacts={split.artifacts} />
    </>
  );
}
