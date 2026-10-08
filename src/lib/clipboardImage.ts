import { readImage } from '@tauri-apps/plugin-clipboard-manager';

/** A clipboard bitmap read + converted to PNG, with a cheap content
 *  fingerprint to tell a fresh screenshot from stale clipboard content. */
export interface ClipboardShot {
  sig: string;
  dataUrl: string;
  base64: string;
}

/** Read the clipboard image (if any) and convert it to a PNG data URL.
 *  Returns null when the clipboard holds no image. Read-only: the clipboard
 *  content stays available for other apps. */
export async function readClipboardShot(): Promise<ClipboardShot | null> {
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

/** `截图-2026-10-08-15-30-00` style timestamp used in pasted-image names. */
export function shotStamp(): string {
  return new Date().toISOString().slice(0, 19).replace(/[T:]/g, '-');
}

/** base64 → byte array, the shape saveAttachmentBytes expects. */
export function base64ToBytes(base64: string): number[] {
  const bin = atob(base64);
  const bytes = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
  return Array.from(bytes);
}
