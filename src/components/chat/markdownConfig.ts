import remarkGfm from 'remark-gfm';
import remarkMath from 'remark-math';
import rehypeKatex from 'rehype-katex';
import { defaultUrlTransform } from 'react-markdown';
import type { PluggableList } from 'unified';
import { remarkFilePaths, SIKU_PATH_SCHEME } from '@/lib/remarkFilePaths';
import { MarkdownCode, MarkdownPre } from './CodeBlock';
import { MarkdownLink } from './FilePathLink';
import { MarkdownImage } from './MarkdownImage';

/** Shared ReactMarkdown wiring for assistant prose (MessageBubble history and
 *  StreamingContent live output must render identically). */
export const assistantRemarkPlugins: PluggableList = [remarkGfm, remarkFilePaths, remarkMath];

export const assistantRehypePlugins: PluggableList = [[rehypeKatex, { throwOnError: false }]];

/** The default transform strips unknown schemes from href/src; keep our
 *  `siku-path:` chip links intact. */
export function markdownUrlTransform(url: string): string {
  if (url.startsWith(SIKU_PATH_SCHEME)) return url;
  return defaultUrlTransform(url);
}

export const assistantMarkdownComponents = {
  a: MarkdownLink,
  img: MarkdownImage,
  code: MarkdownCode,
  pre: MarkdownPre,
};
