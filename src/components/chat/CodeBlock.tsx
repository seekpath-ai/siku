import { useState } from 'react';
import { Check, Copy } from 'lucide-react';

interface CodeBlockProps {
  code: string;
  language?: string;
  inline?: boolean;
  children?: React.ReactNode;
}

const KEYWORDS = /\b(export|import|from|function|return|const|let|var|if|else|for|while|switch|case|break|continue|class|interface|type|extends|implements|async|await|new|this|try|catch|throw|typeof|instanceof|in|of|true|false|null|undefined)\b/g;
const FUNCTIONS = /\b([a-zA-Z_$][a-zA-Z0-9_$]*)(?=\()/g;
const STRINGS = /(".*?"|'.*?'|`.*?`)/g;
const COMMENTS = /(\/\/.*$|\/\*[\s\S]*?\*\/|#.*$)/gm;
const NUMBERS = /\b(\d+(\.\d+)?)\b/g;

function escapeHtml(text: string): string {
  // Quotes are deliberately NOT escaped: they are legal in HTML text content,
  // and the string-detection pass below needs them intact.
  return text
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;');
}

const PLACEHOLDER = '\u0000';

function highlightCode(code: string): string {
  // 1. Escape the WHOLE source up front: the result is injected via
  //    dangerouslySetInnerHTML, so a raw `<`/`>`/`&` (comparisons, generics,
  //    JSX) would otherwise be parsed as markup and silently eaten.
  // 2. Protect comments and strings with placeholders so later passes never
  //    touch their contents.
  // 3. The highlight classes (cb-kw/cb-fn/cb-num) intentionally contain NO
  //    digits — the NUMBERS pass used to match the "400" inside class names
  //    like "text-rose-400" and nest a <span> inside the tag, which the
  //    browser dropped, leaking a visible `400">` fragment into the text.
  const protectedParts: { html: string; cls: string }[] = [];
  const protect = (cls: string) => (m: string) => {
    protectedParts.push({ html: m, cls });
    return `${PLACEHOLDER}K${protectedParts.length - 1}${PLACEHOLDER}`;
  };
  let html = escapeHtml(code).replace(COMMENTS, protect('cb-com')).replace(STRINGS, protect('cb-str'));

  html = html.replace(KEYWORDS, '<span class="cb-kw">$1</span>');
  html = html.replace(FUNCTIONS, '<span class="cb-fn">$1</span>');
  html = html.replace(NUMBERS, '<span class="cb-num">$1</span>');

  // Restore protected parts (already escaped in step 1), wrapped in their
  // highlight class so comments/strings get oneDark colors like the editor.
  // eslint-disable-next-line no-control-regex
  html = html.replace(/\u0000K(\d+)\u0000/g, (_m, idx: string) => {
    const p = protectedParts[Number(idx)];
    return `<span class="${p.cls}">${p.html}</span>`;
  });
  return html;
}

export function CodeBlock({ code, language = 'text', inline }: CodeBlockProps) {
  const [copied, setCopied] = useState(false);

  const handleCopy = async () => {
    try {
      await navigator.clipboard.writeText(code);
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    } catch {
      // ignore
    }
  };

  if (inline) {
    // Inline code: click the chip to copy; the badge floats in the corner so
    // it never shifts the surrounding text layout. box-decoration-clone +
    // generous vertical padding keep a long span that wraps onto two lines
    // visually continuous — by default (slice) the wrapped fragments lose
    // their inner padding and the line gap shows no background, so a single
    // span reads as two separate code chips. Base styling matches the
    // editor's live preview (.cm-live-code): flat white/8 chip, no border.
    return (
      <code
        onClick={handleCopy}
        title="点击复制"
        className="not-prose relative group box-decoration-clone rounded-[3px] px-1.5 py-1 bg-white/[0.08] text-codex-primary font-mono text-[0.9em] cursor-pointer hover:bg-white/[0.14] transition-colors"
      >
        {code}
        <span
          className={`absolute -top-2 -right-2 rounded-full bg-surface border border-surface-hover p-0.5 transition-opacity ${
            copied ? 'opacity-100' : 'opacity-0 group-hover:opacity-100'
          }`}
        >
          {copied
            ? <Check size={9} className="text-accent" />
            : <Copy size={9} className="text-text-secondary" />}
        </span>
      </code>
    );
  }

  // Block code: aligned with the editor's live preview (Obsidian style) —
  // flat translucent background, no border, no header bar; language label +
  // copy button float at the top-right and fade in on hover.
  return (
    <div className="not-prose group relative my-3.5 rounded-md bg-black/30 overflow-hidden">
      <div className="absolute top-1.5 right-2.5 z-10 flex items-center gap-2 opacity-0 group-hover:opacity-100 transition-opacity select-none">
        <span className="font-mono text-[10px] text-text-secondary">{language}</span>
        <button
          onClick={handleCopy}
          className="flex items-center gap-1 rounded px-1.5 py-0.5 border border-white/15 text-[10px] text-text-secondary hover:text-text-primary hover:border-white/30 transition-colors"
        >
          {copied ? <Check size={10} className="text-accent" /> : <Copy size={10} />}
          {copied ? '已复制' : '复制'}
        </button>
      </div>
      <div className="px-2.5 py-1.5 overflow-x-auto">
        <pre className="font-mono text-[13px] leading-relaxed text-gray-200 whitespace-pre">
          <code dangerouslySetInnerHTML={{ __html: highlightCode(code) }} />
        </pre>
      </div>
    </div>
  );
}
