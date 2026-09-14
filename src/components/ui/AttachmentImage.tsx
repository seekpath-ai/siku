import { useEffect, useState } from 'react';
import type { ChatAttachment } from '@/lib/types';

/** Clickable image thumbnail with an in-app lightbox.
 * In the Tauri webview `target="_blank"` on a data: URL has nowhere to
 * open, so attachments preview inline instead (Esc / backdrop click closes). */
export function AttachmentImage({
  att,
  alt,
  className,
}: {
  att: ChatAttachment;
  alt: string;
  className?: string;
}) {
  const [open, setOpen] = useState(false);
  const src = `data:${att.mime};base64,${att.base64}`;

  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') setOpen(false);
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [open]);

  return (
    <>
      <button type="button" onClick={() => setOpen(true)} className="block cursor-zoom-in" title="点击查看大图">
        <img src={src} alt={alt} className={className} />
      </button>
      {open && (
        <div
          className="fixed inset-0 z-[100] flex items-center justify-center bg-black/80 cursor-zoom-out"
          onClick={() => setOpen(false)}
        >
          <img
            src={src}
            alt={alt}
            className="max-w-[92vw] max-h-[92vh] object-contain rounded shadow-2xl"
          />
        </div>
      )}
    </>
  );
}
