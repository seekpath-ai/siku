import { useEffect, useState } from 'react';
import { FileText, FolderOpen } from 'lucide-react';
import { openLocalPath, revealInFileManager, resolveExistingPath } from '@/lib/tauri';
import { SIKU_PATH_SCHEME } from '@/lib/remarkFilePaths';
import { useDialog } from '@/hooks/useDialog';
import { ExternalLink } from '@/components/ui/ExternalLink';

function fileName(path: string): string {
  return path.split(/[\\/]/).filter(Boolean).pop() ?? path;
}

// Candidates repeat across bubbles and grow character-by-character while
// streaming, so existence checks are cached per candidate string and never
// hit the backend twice for the same text.
const resolveCache = new Map<string, Promise<string | null>>();

function resolvePath(candidate: string): Promise<string | null> {
  let p = resolveCache.get(candidate);
  if (!p) {
    p = resolveExistingPath(candidate).catch(() => null);
    resolveCache.set(candidate, p);
  }
  return p;
}

/** Clickable chip for a local path detected by remarkFilePaths: body click
 * opens the file with the OS default app, the folder button reveals it in
 * the file manager. Rendered as a span, not an anchor, so the app-wide
 * external-link guard never sees the `siku-path:` scheme.
 *
 * The greedy candidate may carry glued prose; the backend truncates to a
 * path that exists. Pending/dead candidates render as plain body text —
 * dead ones never become clickable, pending ones upgrade in place. */
export function FilePathLink({ href }: { href?: string }) {
  const { alert } = useDialog();
  const candidate = decodeURIComponent((href ?? '').slice(SIKU_PATH_SCHEME.length));
  const [resolved, setResolved] = useState<string | null>(null);

  useEffect(() => {
    let live = true;
    resolvePath(candidate).then((r) => {
      if (live) setResolved(r);
    });
    return () => {
      live = false;
    };
  }, [candidate]);

  if (!resolved) return <>{candidate}</>;

  // The resolved path is a truncation prefix of the candidate; whatever
  // prose the greedy match swallowed stays visible after the chip.
  const rest = candidate.startsWith(resolved) ? candidate.slice(resolved.length) : '';

  const run = (action: () => Promise<void>, title: string) => {
    action().catch((err) => alert(String(err), title));
  };

  return (
    <>
      <span
        className="inline-flex items-center gap-1 max-w-full align-middle px-1.5 py-0.5 mx-0.5 rounded-md bg-codex-hover/60 border border-codex-border/50 text-[12px] text-codex-primary"
        title={resolved}
      >
        <FileText size={11} className="shrink-0 text-codex-muted" />
        <button
          type="button"
          onClick={() => run(() => openLocalPath(resolved), '打开文件')}
          className="truncate max-w-[260px] hover:underline cursor-pointer"
          title={`打开 ${resolved}`}
        >
          {fileName(resolved)}
        </button>
        <button
          type="button"
          onClick={() => run(() => revealInFileManager(resolved), '在文件夹中显示')}
          className="shrink-0 text-codex-muted hover:text-codex-primary"
          title="在文件夹中显示"
        >
          <FolderOpen size={11} />
        </button>
      </span>
      {rest}
    </>
  );
}

/** react-markdown `a` component: `siku-path:` links become file chips,
 *  everything else keeps the external-link behavior. */
export function MarkdownLink(props: React.AnchorHTMLAttributes<HTMLAnchorElement>) {
  if (props.href?.startsWith(SIKU_PATH_SCHEME)) {
    return <FilePathLink href={props.href} />;
  }
  return <ExternalLink {...props} />;
}
