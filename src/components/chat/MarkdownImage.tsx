import { useEffect, useState } from 'react';
import { convertFileSrc } from '@tauri-apps/api/core';
import { ImageOff } from 'lucide-react';

/** markdown img src → renderable URL: http(s)/data/blob pass through, a
 * local absolute path goes through the asset protocol (scope `**`). Windows
 * paths arrive with backslashes percent-encoded (hast normalizeUri turns
 * `C:\…` into `C:%5C…`) — decode before convertFileSrc. */
function resolveImageSrc(src: string): string {
  if (/^(https?|data|blob|asset):/i.test(src)) return src;
  if (/^[A-Za-z]:%5C/i.test(src)) return convertFileSrc(decodeURIComponent(src));
  if (/^([A-Za-z]:[\\/]|\\\\|\/)/.test(src)) return convertFileSrc(src);
  return src;
}

/** Assistant-bubble image: thumbnail with the same click-to-zoom lightbox as
 * user attachments (Esc / backdrop click closes), plus a failure placeholder
 * so a broken path reads as text instead of a torn icon. */
export function MarkdownImage({ src, alt }: { src?: string; alt?: string }) {
  const [open, setOpen] = useState(false);
  const [failed, setFailed] = useState(false);
  const resolved = src ? resolveImageSrc(src) : '';

  useEffect(() => setFailed(false), [resolved]);
  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') setOpen(false);
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [open]);

  if (!src) return null;
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
      {open && (
        <div
          className="fixed inset-0 z-[100] flex items-center justify-center bg-black/80 cursor-zoom-out"
          onClick={() => setOpen(false)}
        >
          <img
            src={resolved}
            alt={alt ?? ''}
            className="max-w-[92vw] max-h-[92vh] object-contain rounded shadow-2xl"
          />
        </div>
      )}
    </>
  );
}
