import { FileText, FolderOpen } from 'lucide-react';
import { openLocalPath, revealInFileManager } from '@/lib/tauri';
import { SIKU_PATH_SCHEME } from '@/lib/remarkFilePaths';
import { useDialog } from '@/hooks/useDialog';
import { ExternalLink } from '@/components/ui/ExternalLink';

function fileName(path: string): string {
  return path.split(/[\\/]/).filter(Boolean).pop() ?? path;
}

/** Clickable chip for a local path detected by remarkFilePaths: body click
 * opens the file with the OS default app, the folder button reveals it in
 * the file manager. Rendered as a span, not an anchor, so the app-wide
 * external-link guard never sees the `siku-path:` scheme. */
export function FilePathLink({ href }: { href?: string }) {
  const { alert } = useDialog();
  const path = decodeURIComponent((href ?? '').slice(SIKU_PATH_SCHEME.length));

  const run = (action: () => Promise<void>, title: string) => {
    action().catch((err) => alert(String(err), title));
  };

  return (
    <span
      className="inline-flex items-center gap-1 max-w-full align-middle px-1.5 py-0.5 mx-0.5 rounded-md bg-codex-hover/60 border border-codex-border/50 text-[12px] text-codex-primary"
      title={path}
    >
      <FileText size={11} className="shrink-0 text-codex-muted" />
      <button
        type="button"
        onClick={() => run(() => openLocalPath(path), '打开文件')}
        className="truncate max-w-[260px] hover:underline cursor-pointer"
        title={`打开 ${path}`}
      >
        {fileName(path)}
      </button>
      <button
        type="button"
        onClick={() => run(() => revealInFileManager(path), '在文件夹中显示')}
        className="shrink-0 text-codex-muted hover:text-codex-primary"
        title="在文件夹中显示"
      >
        <FolderOpen size={11} />
      </button>
    </span>
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
