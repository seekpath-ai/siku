import { useEffect, useState, type ReactNode } from 'react';
import { FolderOpen } from 'lucide-react';
import { artifactResolve, openLocalPath, revealInFileManager } from '@/lib/tauri';
import { artifactUriLabel, isArtifactUri, safeDecodeUri } from '@/lib/artifact';
import { useDialog } from '@/hooks/useDialog';
import { useSyncTick } from '@/stores/syncTickStore';
import { ArtifactTypeIcon } from './artifactVisual';
import { artifactExt } from './artifactMeta';

/** Chip for a `siku://artifact/<hash>.<ext>` link: resolves the blob on THIS
 *  device (it may still be arriving from a peer) and opens/reveals it with the
 *  OS. Rendered as a span, not an anchor, so the external-link guard never
 *  sees the custom scheme. */
export function ArtifactLink({ href, children }: { href?: string; children?: ReactNode }) {
  const { alert } = useDialog();
  const uri = safeDecodeUri(href ?? '');
  const [path, setPath] = useState<string | null>(null);
  const [failed, setFailed] = useState(false);
  const syncTick = useSyncTick((s) => s.tick);

  useEffect(() => {
    let live = true;
    setPath(null);
    setFailed(false);
    if (!isArtifactUri(uri)) return;
    artifactResolve(uri)
      .then((p) => {
        if (live) setPath(p);
      })
      .catch(() => {
        if (live) setFailed(true);
      });
    return () => {
      live = false;
    };
  }, [uri, syncTick]);

  if (!isArtifactUri(uri)) return null;

  const label = typeof children === 'string' ? children : artifactUriLabel(uri);
  const run = (action: () => Promise<void>, title: string) => {
    action().catch((err) => alert(String(err), { title, kind: 'error' }));
  };

  return (
    <span className="group inline-flex items-center gap-1 max-w-full align-middle px-1.5 py-0.5 mx-0.5 rounded-md bg-codex-hover/60 border border-codex-border/50 text-[12px] text-codex-primary">
      <ArtifactTypeIcon ext={artifactExt(uri)} size={12} />
      <button
        type="button"
        onClick={() => {
          if (path) {
            run(() => openLocalPath(path), '打开产物');
          } else {
            alert(failed ? '产物尚未同步到本机，请稍后重试' : '产物正在准备，请稍后重试', {
              title: '打开产物',
              kind: failed ? 'warning' : 'info',
            });
          }
        }}
        className={`truncate max-w-[260px] hover:underline cursor-pointer ${path ? '' : 'text-codex-muted'}`}
        title={path ?? (failed ? '尚未同步' : '解析中…')}
      >
        {label}
      </button>
      {path && (
        <button
          type="button"
          onClick={() => run(() => revealInFileManager(path), '在文件夹中显示')}
          className="shrink-0 text-codex-muted opacity-100 hover:text-codex-primary sm:opacity-0 sm:group-hover:opacity-100 sm:focus-visible:opacity-100"
          title="在文件夹中显示"
        >
          <FolderOpen size={11} />
        </button>
      )}
    </span>
  );
}
