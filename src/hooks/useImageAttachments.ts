import { useState, useRef, useEffect, useCallback } from 'react';
import { readImage } from '@tauri-apps/plugin-clipboard-manager';
import { screenshotStart } from '@/lib/tauri';

/** Local image attachment staged in an input box (sent as ChatAttachment). */
export interface ImageAttachment {
  kind: 'image';
  id: string;
  name: string;
  mime: string;
  base64: string;
  previewUrl: string;
}

export function fileToBase64(file: File): Promise<{ base64: string; mime: string; previewUrl: string }> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => {
      const result = reader.result as string;
      const [header, base64] = result.split(',');
      const mime = header.match(/data:(.+);base64/)?.[1] || file.type || 'image/png';
      resolve({ base64, mime, previewUrl: result });
    };
    reader.onerror = reject;
    reader.readAsDataURL(file);
  });
}

interface ClipboardShot {
  /** Cheap content fingerprint, to tell a fresh screenshot from stale clipboard content. */
  sig: string;
  dataUrl: string;
  base64: string;
}

/** Read the clipboard image (if any) and convert it to a PNG data URL. */
async function readClipboardShot(): Promise<ClipboardShot | null> {
  try {
    const img = await readImage();
    const { width, height } = await img.size();
    const rgba = await img.rgba();
    if (!width || !height || rgba.length === 0) return null;
    let sum = 0;
    for (let i = 0; i < rgba.length; i += 997) sum = (sum + rgba[i]) & 0xffffff;
    const canvas = document.createElement('canvas');
    canvas.width = width;
    canvas.height = height;
    const ctx = canvas.getContext('2d');
    if (!ctx) return null;
    ctx.putImageData(new ImageData(new Uint8ClampedArray(rgba), width, height), 0, 0);
    const dataUrl = canvas.toDataURL('image/png');
    return { sig: `${width}x${height}:${rgba.length}:${sum}`, dataUrl, base64: dataUrl.split(',')[1] ?? '' };
  } catch {
    return null; // clipboard holds no image
  }
}

/** Window during which a fresh clipboard image is treated as the screenshot
 * the user was asked to take (avoids attaching unrelated clipboard images). */
const SCREENSHOT_TIMEOUT_MS = 120_000;

interface UseImageAttachmentsOptions {
  /** Max staged images; additions beyond this are rejected via onError. */
  max?: number;
  /** When true, the Ctrl+Shift+S shortcut (if enabled) does not fire. */
  disabled?: boolean;
  /** Register the global Ctrl+Shift+S shortcut. Only ONE mounted input may
   * enable this, otherwise a single keypress arms several screenshot flows. */
  enableShortcut?: boolean;
  /** Called after a screenshot lands (e.g. to refocus the input). */
  onAttached?: () => void;
  /** Surface user-facing errors (screenshot failure, max exceeded). */
  onError?: (message: string, title?: string) => void;
}

/** Shared image-attachment machinery for message inputs: staging, paste,
 * and the OS-screenshot flow (scissors button → snipping tool → clipboard
 * image auto-attached). Used by the main chat MessageInput and the pet panel. */
export function useImageAttachments({
  max,
  disabled,
  enableShortcut,
  onAttached,
  onError,
}: UseImageAttachmentsOptions = {}) {
  const [images, setImages] = useState<ImageAttachment[]>([]);
  const shotPending = useRef<{ since: number; prevSig: string | null } | null>(null);
  const [shotArmed, setShotArmed] = useState(false);

  const addImageData = useCallback(
    (data: { name: string; mime: string; base64: string; previewUrl: string }) => {
      setImages((prev) => {
        if (max !== undefined && prev.length >= max) {
          onError?.(`最多附加 ${max} 张图片`, '图片附件');
          return prev;
        }
        return [
          ...prev,
          {
            kind: 'image' as const,
            id: `img_${Date.now()}_${Math.random().toString(36).slice(2, 8)}`,
            ...data,
          },
        ];
      });
    },
    [max, onError]
  );

  const addImageFile = useCallback(
    async (file: File) => {
      try {
        const { base64, mime, previewUrl } = await fileToBase64(file);
        addImageData({ name: file.name, mime, base64, previewUrl });
      } catch (err) {
        console.error('Failed to read image file:', err);
      }
    },
    [addImageData]
  );

  const removeImage = useCallback((id: string) => {
    setImages((prev) => prev.filter((a) => a.id !== id));
  }, []);

  const clearImages = useCallback(() => setImages([]), []);

  // Screenshot flow: the scissors button launches the OS snipping tool; a
  // fresh clipboard image within SCREENSHOT_TIMEOUT_MS is attached
  // automatically. Checked on window focus AND by polling, because some
  // tools (e.g. spectacle -rb) never steal window focus.
  const checkClipboardShot = useCallback(async () => {
    const pending = shotPending.current;
    if (!pending) return;
    if (Date.now() - pending.since > SCREENSHOT_TIMEOUT_MS) {
      shotPending.current = null;
      setShotArmed(false);
      return;
    }
    const shot = await readClipboardShot();
    if (!shot || shot.sig === pending.prevSig) return;
    shotPending.current = null;
    setShotArmed(false);
    const stamp = new Date().toISOString().slice(0, 19).replace(/[T:]/g, '-');
    addImageData({
      name: `截图-${stamp}.png`,
      mime: 'image/png',
      base64: shot.base64,
      previewUrl: shot.dataUrl,
    });
    onAttached?.();
  }, [addImageData, onAttached]);

  useEffect(() => {
    if (!shotArmed) return;
    window.addEventListener('focus', checkClipboardShot);
    const timer = setInterval(checkClipboardShot, 2000);
    return () => {
      window.removeEventListener('focus', checkClipboardShot);
      clearInterval(timer);
    };
  }, [shotArmed, checkClipboardShot]);

  const startScreenshot = useCallback(async () => {
    // Remember the current clipboard content so only a NEW image attaches.
    const prev = await readClipboardShot();
    try {
      await screenshotStart();
      shotPending.current = { since: Date.now(), prevSig: prev?.sig ?? null };
      setShotArmed(true);
    } catch (err) {
      onError?.(String(err), '截图');
    }
  }, [onError]);

  // Optional in-app shortcut: Ctrl+Shift+S triggers the screenshot flow.
  useEffect(() => {
    if (!enableShortcut) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.ctrlKey && e.shiftKey && e.key.toLowerCase() === 's') {
        e.preventDefault();
        if (!disabled) startScreenshot();
      }
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [enableShortcut, disabled, startScreenshot]);

  // Attach pasted images; text paste falls through to the default handler.
  const handlePaste = useCallback(
    async (e: React.ClipboardEvent) => {
      const files = e.clipboardData?.files;
      if (files) {
        for (const file of Array.from(files)) {
          if (file.type.startsWith('image/')) {
            e.preventDefault();
            await addImageFile(file);
            return;
          }
        }
      }
      // WebKitGTK (Linux) often reports NO files for a clipboard image, so
      // the loop above silently does nothing. When the clipboard carries no
      // text either, fall back to the native clipboard API — that is how a
      // screenshot pasted with Ctrl+V still lands as an attachment.
      const items = e.clipboardData?.items;
      const hasText = items ? Array.from(items).some((i) => i.kind === 'string') : false;
      if (hasText) return; // ordinary text paste, let it through
      const shot = await readClipboardShot();
      if (shot) {
        e.preventDefault();
        const stamp = new Date().toISOString().slice(0, 19).replace(/[T:]/g, '-');
        addImageData({
          name: `粘贴-${stamp}.png`,
          mime: 'image/png',
          base64: shot.base64,
          previewUrl: shot.dataUrl,
        });
      }
    },
    [addImageFile, addImageData]
  );

  return {
    images,
    addImageData,
    addImageFile,
    removeImage,
    clearImages,
    shotArmed,
    startScreenshot,
    handlePaste,
  };
}
