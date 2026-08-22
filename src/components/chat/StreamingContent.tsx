import ReactMarkdown from 'react-markdown';
import remarkGfm from 'remark-gfm';
import remarkMath from 'remark-math';
import rehypeKatex from 'rehype-katex';
import { CodeBlock } from './CodeBlock';
import { ExternalLink } from '@/components/ui/ExternalLink';

interface Props {
  content: string;
}

export function StreamingContent({ content }: Props) {
  return (
    <div className="prose prose-sm prose-invert max-w-none [&>*:first-child]:mt-0">
      <ReactMarkdown
        remarkPlugins={[remarkGfm, remarkMath]}
        rehypePlugins={[[rehypeKatex, { throwOnError: false }]]}
        components={{
          a: ExternalLink,
          code({ className, children }) {
            const match = /language-(\w+)/.exec(className || '');
            const language = match ? match[1] : '';
            const code = String(children).replace(/\n$/, '');
            const isInline = !match && !code.includes('\n');

            return (
              <CodeBlock code={code} language={language || 'text'} inline={isInline}>
                {children}
              </CodeBlock>
            );
          },
        }}
      >
        {content}
      </ReactMarkdown>
      <span className="inline-block w-1.5 h-4 bg-codex-accent animate-pulse ml-0.5 align-middle rounded-sm" />
    </div>
  );
}
