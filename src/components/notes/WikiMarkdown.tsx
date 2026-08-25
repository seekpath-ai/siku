import { useMemo, useCallback, useState, useEffect, useRef } from 'react';
import ReactMarkdown from 'react-markdown';
import remarkGfm from 'remark-gfm';
import remarkMath from 'remark-math';
import rehypeKatex from 'rehype-katex';
import { Check, Copy } from 'lucide-react';
import { useNavigate } from '@tanstack/react-router';
import { emit } from '@tauri-apps/api/event';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { MarkdownCode, MarkdownPre } from '@/components/chat/CodeBlock';
import { open } from '@tauri-apps/plugin-shell';
import { resolveImageUrl } from '@/lib/imageCache';
import { parseReaderUrl } from '@/lib/evidence';
import { useEvidenceStore } from '@/stores/evidenceStore';
import type { Note } from '@/lib/types';

interface Props {
  content: string;
  notes: Note[];
  onNavigate: (id: string) => void;
  /** Called when an unresolved [[link]] is clicked (to create the note). */
  onCreateLink?: (title: string) => void;
  className?: string;
  /** Absolute path to the vault attachments directory. */
  attachmentsDir?: string;
}

const WIKI_LINK_RE = /\[\[([^\]]+?)\]\]/g;
const EMBED_RE = /!\[\[([^\]]+?)\]\]/g;

// ReactMarkdown v9 strips unknown protocols by default. We need `note://`,
// `note-create://` and `siku-reader://` preserved so our custom link
// component can handle navigation / evidence highlighting.
const ALLOWED_URL_PROTOCOLS = /^(https?|mailto|tel|note|note-create|siku-reader)$/i;
function allowCustomProtocols(url: string): string | undefined {
  const colon = url.indexOf(':');
  if (colon === -1) return url;
  return ALLOWED_URL_PROTOCOLS.test(url.slice(0, colon)) ? url : undefined;
}

// Convert soft line breaks inside paragraphs into hard `<br>` breaks so that
// reading view matches the edit view line-by-line (Obsidian-style strict line
// breaks). This only affects text nodes; code blocks and other fenced content
// keep their original line breaks.
function remarkStrictLineBreaks() {
  return (tree: unknown) => {
    const walk = (node: { type?: string; children?: unknown[] }) => {
      if (!Array.isArray(node.children)) return;
      let i = 0;
      while (i < node.children.length) {
        const child = node.children[i] as { type?: string; value?: string };
        if (child.type === 'text' && typeof child.value === 'string' && child.value.includes('\n')) {
          const parts = child.value.split('\n');
          const replacements: { type: string; value?: string }[] = [];
          parts.forEach((part, idx) => {
            if (part) replacements.push({ type: 'text', value: part });
            if (idx < parts.length - 1) replacements.push({ type: 'break' });
          });
          node.children.splice(i, 1, ...replacements);
          i += replacements.length;
        } else {
          walk(child as { type?: string; children?: unknown[] });
          i += 1;
        }
      }
    };
    walk(tree as { type?: string; children?: unknown[] });
  };
}

// Table cells (reading view): hovering a cell reveals a small copy button in
// its top-right corner that copies the cell's plain text. The button is
// icon-only so it never pollutes the copied content.
type CellProps = React.TdHTMLAttributes<HTMLTableCellElement> & { node?: unknown };

function CopyableCell({ tag, node: _node, children, className, ...rest }: CellProps & { tag: 'td' | 'th' }) {
  const ref = useRef<HTMLTableCellElement>(null);
  const [copied, setCopied] = useState(false);

  const handleCopy = async (e: React.MouseEvent) => {
    e.stopPropagation();
    const text = ref.current?.innerText.trim() ?? '';
    if (!text) return;
    try {
      await navigator.clipboard.writeText(text);
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    } catch {
      // ignore
    }
  };

  const Tag = tag;
  return (
    <Tag ref={ref} {...rest} className={`relative group/cell ${className ?? ''}`}>
      {children}
      <button
        onClick={handleCopy}
        title="复制单元格内容"
        className={`absolute top-0.5 right-0.5 rounded bg-surface border border-surface-hover p-0.5 transition-opacity ${
          copied ? 'opacity-100' : 'opacity-0 group-hover/cell:opacity-100'
        }`}
      >
        {copied
          ? <Check size={10} className="text-accent" />
          : <Copy size={10} className="text-text-secondary" />}
      </button>
    </Tag>
  );
}

const TdComponent = (props: CellProps) => <CopyableCell tag="td" {...props} />;
const ThComponent = (props: CellProps) => <CopyableCell tag="th" {...props} />;

function resolveWikiTarget(raw: string, notes: Note[]): { targetId: string | null; display: string } {
  const aliasIdx = raw.indexOf('|');
  const title = (aliasIdx >= 0 ? raw.slice(0, aliasIdx) : raw).trim();
  const display = (aliasIdx >= 0 ? raw.slice(aliasIdx + 1) : raw).trim();
  const target = notes.find(
    (n) =>
      n.title.trim().toLowerCase() === title.toLowerCase() ||
      (() => {
        try {
          const aliases = JSON.parse(n.aliases || '[]');
          return Array.isArray(aliases) && aliases.some((a: string) => a.trim().toLowerCase() === title.toLowerCase());
        } catch {
          return false;
        }
      })()
  );
  return { targetId: target?.id ?? null, display: display || title };
}

// Images: remote URLs are cached by the Rust backend; local/attachment paths
// are resolved against attachmentsDir and loaded through the asset protocol.
// A real component (not an inline callback) so hooks are legal.
function MdImage({
  attachmentsDir,
  src,
  alt,
  ...rest
}: React.ImgHTMLAttributes<HTMLImageElement> & { attachmentsDir?: string }) {
  const [resolvedSrc, setResolvedSrc] = useState<string | undefined>(src);
  const [failed, setFailed] = useState(false);

  useEffect(() => {
    if (!src) {
      setResolvedSrc(undefined);
      return;
    }
    let active = true;
    setFailed(false);
    resolveImageUrl(src, { attachmentsDir })
      .then((url) => {
        if (active) setResolvedSrc(url);
      })
      .catch(() => {
        if (active) setFailed(true);
      });
    return () => {
      active = false;
    };
  }, [src, attachmentsDir]);

  if (failed || !resolvedSrc) {
    return (
      <span className="inline-block px-2 py-1 rounded bg-surface-hover text-text-secondary/60 text-xs">
        {failed ? '[图片加载失败]' : '[图片]'}
      </span>
    );
  }

  return (
    <img
      src={resolvedSrc}
      alt={alt}
      {...rest}
      onError={() => setFailed(true)}
      className={`max-w-full rounded ${rest.className ?? ''}`}
    />
  );
}

export function WikiMarkdown({ content, notes, onNavigate, onCreateLink, className, attachmentsDir }: Props) {
  const navigate = useNavigate();
  const processed = useMemo(() => {
    // 1. Embeds: ![[note]] → inline the target's content (one level deep).
    let text = content.replace(EMBED_RE, (_match, raw: string) => {
      const { targetId } = resolveWikiTarget(raw, notes);
      if (!targetId) return `![[${raw}]]`;
      const target = notes.find((n) => n.id === targetId);
      return target ? `\n\n${target.content}\n\n` : `![[${raw}]]`;
    });
    // 2. Wiki links → note:// or note-create:// links.
    text = text.replace(WIKI_LINK_RE, (_match, raw: string) => {
      const { targetId, display } = resolveWikiTarget(raw, notes);
      if (!targetId) {
        return `[${display}](note-create://${encodeURIComponent(display)})`;
      }
      return `[${display}](note://${targetId})`;
    });
    return text;
  }, [content, notes]);

  const LinkComponent = useCallback(
    (props: React.AnchorHTMLAttributes<HTMLAnchorElement>) => {
      const { href, children } = props;
      if (href?.startsWith('note://')) {
        const id = href.slice(7);
        const target = notes.find((n) => n.id === id);
        const preview = target
          ? `${target.title}\n\n${(target.content_plain || '').slice(0, 200)}`
          : '';
        return (
          <a
            {...props}
            href="#"
            title={preview || undefined}
            onClick={(e) => {
              e.preventDefault();
              onNavigate(id);
            }}
            className="text-primary hover:underline cursor-pointer"
          >
            {children}
          </a>
        );
      }
      if (href?.startsWith('note-create://')) {
        const title = decodeURIComponent(href.slice('note-create://'.length));
        return (
          <a
            {...props}
            href="#"
            onClick={(e) => {
              e.preventDefault();
              onCreateLink?.(title);
            }}
            title={`创建笔记「${title}」`}
            className="text-red-400/80 border-b border-dashed border-red-400/60 hover:text-red-400 hover:border-red-400 cursor-pointer"
          >
            {children}
          </a>
        );
      }
      // Evidence deep links saved into notes by the pet panel: jump to the
      // reader and highlight the quoted passage (same channel as the pet
      // citation badges).
      if (href?.startsWith('siku-reader://')) {
        const target = parseReaderUrl(href);
        if (!target) return <span>{children}</span>;
        return (
          <a
            {...props}
            href="#"
            title={`在 PDF 中定位证据\n\n${target.exact}`}
            onClick={(e) => {
              e.preventDefault();
              // The reader route only exists in the main window's router —
              // note windows are slim router-less windows, so forward the
              // request there (same channel the detached pet chat uses).
              if (getCurrentWindow().label === 'main') {
                useEvidenceStore.getState().requestHighlight({
                  paperId: target.paperId,
                  page: target.page,
                  exact: target.exact,
                });
                navigate({ to: '/reader/$paperId', params: { paperId: target.paperId } });
              } else {
                emit('pet:evidence-highlight', {
                  paperId: target.paperId,
                  page: target.page,
                  exact: target.exact,
                }).catch(() => {});
              }
            }}
            className="text-primary hover:underline cursor-pointer"
          >
            {children}
          </a>
        );
      }
      // External links: open in the system browser, never inside the app.
      if (href?.startsWith('http://') || href?.startsWith('https://')) {
        return (
          <a
            {...props}
            href={href}
            onClick={(e) => {
              e.preventDefault();
              open(href).catch(() => {
                // Fallback when running outside Tauri (browser dev/tests).
                window.open(href, '_blank', 'noopener');
              });
            }}
            className="text-primary hover:underline"
          />
        );
      }
      return <a {...props} className="text-primary hover:underline" />;
    },
    [onNavigate, onCreateLink, notes, navigate]
  );

  // Images are rendered by the MdImage component (remote caching + asset
  // protocol resolution); this wrapper just injects attachmentsDir.
  const ImageComponent = useCallback(
    (props: React.ImgHTMLAttributes<HTMLImageElement>) => (
      <MdImage {...props} attachmentsDir={attachmentsDir} />
    ),
    [attachmentsDir]
  );

  // Code blocks: shared react-markdown wiring from CodeBlock — block vs
  // inline is decided by structure (pre > code vs bare code), so a
  // language-less single-line fence is never mistaken for inline code.

  return (
    <ReactMarkdown
      remarkPlugins={[remarkGfm, remarkMath, remarkStrictLineBreaks]}
      rehypePlugins={[[rehypeKatex, { throwOnError: false }]]}
      components={{ a: LinkComponent, code: MarkdownCode, pre: MarkdownPre, img: ImageComponent, td: TdComponent, th: ThComponent }}
      urlTransform={allowCustomProtocols}
      className={className}
    >
      {processed}
    </ReactMarkdown>
  );
}
