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
