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

function fileUrlToLocalPath(url: string): string | null {
  if (!/^file:/i.test(url)) return null;
  try {
    const parsed = new URL(url);
    const pathname = decodeURIComponent(parsed.pathname);
    if (parsed.hostname && parsed.hostname !== 'localhost') {
      return `\\\\${parsed.hostname}${pathname.replace(/\//g, '\\')}`;
    }
    return /^\/[A-Za-z]:[\\/]/.test(pathname) ? pathname.slice(1) : pathname;
  } catch {
    return null;
  }
}

/** The default transform strips unknown schemes from href/src; keep our
 *  `siku-path:` chip links intact. Also pass local absolute paths through:
 *  tools like paper_snapshot return them as img src, and the default would
 *  strip a Windows path as an unknown "c:" protocol. Note backslashes arrive
 *  percent-encoded (hast normalizeUri): `C:\…` shows up here as `C:%5C…`.
 *  File URLs are converted before that filter so completed Markdown images do
 *  not disappear after being visible as plain text during streaming. */
export function markdownUrlTransform(url: string): string {
  if (url.startsWith(SIKU_PATH_SCHEME)) return url;
  const filePath = fileUrlToLocalPath(url);
  if (filePath) return filePath;
  if (/^([A-Za-z]:[\\/]|[A-Za-z]:%5C|\\\\|\/(?!\/))/i.test(url)) return url;
  return defaultUrlTransform(url);
}

export const assistantMarkdownComponents = {
  a: MarkdownLink,
  img: MarkdownImage,
  code: MarkdownCode,
  pre: MarkdownPre,
};
