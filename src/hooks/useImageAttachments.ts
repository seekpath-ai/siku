import { useState, useRef, useEffect, useCallback } from 'react';
import { screenshotStart } from '@/lib/tauri';
import { readClipboardShot, shotStamp } from '@/lib/clipboardImage';

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

/** Window during which a fresh clipboard image is treated as the screenshot
 * the user was asked to take (avoids attaching unrelated clipboard images). */
const SCREENSHOT_TIMEOUT_MS = 120_000;

/** Grace after focus returns with no fresh clipboard image before the armed
 *  screenshot flow is treated as canceled. Covers slow clipboard writes. */
const SHOT_CANCEL_GRACE_MS = 2500;

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
  /** Set when the window regains focus while armed: a focus-stealing snipping
   *  tool has closed. If no fresh clipboard image appears within a short
   *  grace period, the user canceled the shot (Esc) — disarm the flow.
   *  (Focus-less tools like `spectacle -rb` never set this; they keep
   *  relying on polling + timeout.) */
  const shotFocusReturn = useRef<number | null>(null);

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
    // Claim the pending slot SYNCHRONOUSLY, before any await: reading the
    // clipboard (RGBA decode + canvas encode) can take hundreds of ms for a
    // large screenshot, and a focus event plus a polling tick entering
    // together would otherwise both pass the null check and attach twice.
    shotPending.current = null;
    if (Date.now() - pending.since > SCREENSHOT_TIMEOUT_MS) {
      shotFocusReturn.current = null;
      setShotArmed(false);
      return;
    }
    const shot = await readClipboardShot();
    if (!shot || shot.sig === pending.prevSig) {
      // Focus came back to us without a fresh clipboard image: the snipping
      // tool closed without capturing (Esc / cancel). Allow a grace window
      // for slow clipboard writes (the success path can lag focus), then
      // disarm so the button doesn't stay stuck in the armed state.
      const returnedAt = shotFocusReturn.current;
      if (returnedAt !== null && Date.now() - returnedAt > SHOT_CANCEL_GRACE_MS) {
        shotFocusReturn.current = null;
        setShotArmed(false);
        return;
      }
      shotPending.current = pending; // no fresh shot — hand the claim back
      return;
    }
    shotFocusReturn.current = null;
    setShotArmed(false);
    addImageData({
      name: `截图-${shotStamp()}.png`,
      mime: 'image/png',
      base64: shot.base64,
      previewUrl: shot.dataUrl,
    });
    onAttached?.();
  }, [addImageData, onAttached]);

  useEffect(() => {
    if (!shotArmed) return;
    const onFocus = () => {
      shotFocusReturn.current ??= Date.now();
      void checkClipboardShot();
    };
    window.addEventListener('focus', onFocus);
    const timer = setInterval(checkClipboardShot, 2000);
    return () => {
      window.removeEventListener('focus', onFocus);
      clearInterval(timer);
    };
  }, [shotArmed, checkClipboardShot]);

  const startScreenshot = useCallback(async () => {
    // A second click while armed = cancel the pending shot (the OS tool
    // gives us no cancellation event otherwise).
    if (shotArmed) {
      shotPending.current = null;
      shotFocusReturn.current = null;
      setShotArmed(false);
      return;
    }
    // Remember the current clipboard content so only a NEW image attaches.
    // Window state is the user's deliberate choice: a visible window means
    // "may appear in the shot", a minimized one means they already cleared
    // it out of the way — never minimize/restore on their behalf.
    const prev = await readClipboardShot();
    try {
      await screenshotStart();
      shotPending.current = { since: Date.now(), prevSig: prev?.sig ?? null };
      setShotArmed(true);
    } catch (err) {
      onError?.(String(err), '截图');
    }
  }, [shotArmed, onError]);

  // Optional in-app shortcut: Ctrl+Shift+S triggers the screenshot flow.
  // Also responds to the OS-global hotkey (registered by the backend, fired
  // even while minimized), forwarded by AppShell as 'siku:screenshot-hotkey'.
  useEffect(() => {
    if (!enableShortcut) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.ctrlKey && e.shiftKey && e.key.toLowerCase() === 's') {
        e.preventDefault();
        if (!disabled) startScreenshot();
      }
    };
    const onGlobalHotkey = () => {
      if (!disabled) startScreenshot();
    };
    window.addEventListener('keydown', onKey);
    window.addEventListener('siku:screenshot-hotkey', onGlobalHotkey);
    return () => {
      window.removeEventListener('keydown', onKey);
      window.removeEventListener('siku:screenshot-hotkey', onGlobalHotkey);
    };
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
        addImageData({
          name: `粘贴-${shotStamp()}.png`,
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
