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
