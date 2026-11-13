import { useEffect, useState } from 'react';
import { convertFileSrc } from '@tauri-apps/api/core';
import { ImageOff, Loader2 } from 'lucide-react';
import { artifactResolve } from '@/lib/tauri';
import { isArtifactUri, safeDecodeUri } from '@/lib/artifact';
import { useSyncTick } from '@/stores/syncTickStore';
import { ImageLightbox } from '@/components/ui/Modal';

/** Git Bash (the agent's preferred shell on Windows) prints MSYS-style
 * paths like `/c/Users/…` and the model quotes them back verbatim in
 * `![](…)`. Map the drive prefix back to `C:/…`; elsewhere `/c/…` can be a
 * real POSIX path, so leave it alone. */
function normalizeMsysPath(src: string): string {
  if (typeof navigator !== 'undefined' && !navigator.userAgent.includes('Windows')) return src;
  return src.replace(/^\/([A-Za-z])(?=[\\/])/, '$1:');
}

/** markdown img src → renderable URL: http(s)/data/blob pass through, a
 * local absolute path goes through the asset protocol (scope `**`). Windows
 * paths arrive with backslashes percent-encoded (hast normalizeUri turns
 * `C:\…` into `C:%5C…`) — decode before convertFileSrc. */
function resolveImageSrc(src: string): string {
  const s = normalizeMsysPath(src);
  if (/^(https?|data|blob|asset):/i.test(s)) return s;
  if (/^[A-Za-z]:%5C/i.test(s)) return convertFileSrc(decodeURIComponent(s));
  if (/^([A-Za-z]:[\\/]|\\\\|\/)/.test(s)) return convertFileSrc(s);
  return s;
}

/** Assistant-bubble image: thumbnail with the same click-to-zoom lightbox as
 * user attachments (Esc / backdrop click closes), plus a failure placeholder
 * so a broken path reads as text instead of a torn icon. */
export function MarkdownImage({ src, alt }: { src?: string; alt?: string }) {
  const [open, setOpen] = useState(false);
  const [failed, setFailed] = useState(false);
  const artifact = isArtifactUri(src);
  const [artifactPath, setArtifactPath] = useState<string | null>(null);
  const syncTick = useSyncTick((s) => s.tick);

  useEffect(() => {
    let live = true;
    setArtifactPath(null);
    setFailed(false);
    if (!isArtifactUri(src)) return;
    artifactResolve(safeDecodeUri(src))
      .then((p) => {
        if (live) setArtifactPath(p);
      })
      .catch(() => {
        if (live) setFailed(true);
      });
    return () => {
      live = false;
    };
  }, [src, syncTick]);

  const resolved = artifact
    ? artifactPath
      ? convertFileSrc(artifactPath)
      : ''
    : src
      ? resolveImageSrc(src)
      : '';

  useEffect(() => setFailed(false), [resolved]);

  if (!src) return null;
  if (artifact && !resolved && !failed) {
    return (
      <span className="inline-flex items-center gap-1 px-2 py-1 rounded-md border border-codex-border/50 text-[11px] text-codex-muted">
        <Loader2 size={11} className="shrink-0 animate-spin" />
        产物同步中…
      </span>
    );
  }
  if (failed) {
    return (
      <span className="inline-flex items-center gap-1 px-2 py-1 rounded-md border border-codex-border/50 text-[11px] text-codex-muted">
        <ImageOff size={11} className="shrink-0" />
        图片不可用：{src}
      </span>
    );
  }

  return (
    <>
      <button type="button" onClick={() => setOpen(true)} className="block cursor-zoom-in" title="点击查看大图">
        <img
          src={resolved}
          alt={alt ?? ''}
          onError={() => setFailed(true)}
          className="max-w-[240px] max-h-[180px] object-cover rounded-lg border border-codex-border/50"
        />
      </button>
      {open && <ImageLightbox src={resolved} alt={alt} onClose={() => setOpen(false)} />}
    </>
  );
}
