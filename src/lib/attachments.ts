import type { ChatAttachment } from '@/lib/types';

/** Parse the JSON-serialized ChatAttachment[] stored on a chat message. */
export function parseAttachments(json: string | null): ChatAttachment[] {
  if (!json) return [];
  try {
    const parsed = JSON.parse(json);
    return Array.isArray(parsed) ? parsed : [];
  } catch {
    return [];
  }
}

/** Inline image with a base64 payload (user turns, and legacy rows). */
export function isInlineImage(att: ChatAttachment): boolean {
  return att.kind !== 'file' && typeof att.base64 === 'string' && att.base64.length > 0;
}

/** Outbound file registered by a remote agent turn (assistant rows). */
export function isOutboundFile(att: ChatAttachment): boolean {
  return att.kind === 'file';
}

export const IMAGE_EXTS = new Set(['png', 'jpg', 'jpeg', 'gif', 'webp', 'bmp']);

/** Picker filter list shared by the main chat and pet conversation attach
 * buttons (kept in sync so both surfaces accept the same files). */
export const ATTACH_FILTERS = [
  { name: '所有文件', extensions: ['*'] },
  { name: '图片', extensions: ['png', 'jpg', 'jpeg', 'gif', 'webp', 'bmp'] },
  { name: '文档', extensions: ['pdf', 'docx', 'xlsx', 'pptx'] },
  { name: '文本文件', extensions: ['txt', 'md', 'json', 'ts', 'tsx', 'js', 'rs', 'py', 'css', 'html', 'yml', 'yaml', 'toml', 'csv', 'xml'] },
];

export interface StagedImage {
  name: string;
  mime: string;
  base64: string;
  previewUrl: string;
}

/** Dispatch picked/dropped paths by extension: images are read inline as
 * base64 (vision models), everything else goes to text extraction. Returns
 * one failure message per path that could not be attached. */
export async function dispatchAttachmentPaths(
  paths: string[],
  opts: {
    imageExists: (name: string) => boolean;
    addImage: (image: StagedImage) => void;
    addTextPath: (path: string) => Promise<string | null>;
  }
): Promise<string[]> {
  const { readImageFile } = await import('@/lib/tauri');
  const failures: string[] = [];
  for (const path of paths) {
    const name = path.split(/[\\/]/).pop() || path;
    const ext = name.split('.').pop()?.toLowerCase() ?? '';
    if (IMAGE_EXTS.has(ext)) {
      if (opts.imageExists(name)) continue;
      try {
        const att = await readImageFile(path);
        opts.addImage({
          name: att.name || name,
          mime: att.mime,
          base64: att.base64,
          previewUrl: `data:${att.mime};base64,${att.base64}`,
        });
      } catch (err) {
        failures.push(`${name}: ${err}`);
      }
    } else {
      const err = await opts.addTextPath(path);
      if (err) failures.push(err);
    }
  }
  return failures;
}
