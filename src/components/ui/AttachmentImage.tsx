import { useState } from 'react';
import type { ChatAttachment } from '@/lib/types';
import { ImageLightbox } from './Modal';

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
  // `base64` is absent on outbound-file rows (those render as a file card).
  const src = att.base64 ? `data:${att.mime};base64,${att.base64}` : '';


  if (!src) return null;

  return (
    <>
      <button type="button" onClick={() => setOpen(true)} className="block cursor-zoom-in" title="点击查看大图">
        <img src={src} alt={alt} className={className} />
      </button>
      {open && <ImageLightbox src={src} alt={alt} onClose={() => setOpen(false)} />}
    </>
  );
}
