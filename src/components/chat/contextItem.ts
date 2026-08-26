/** One item picked in the context picker. `content` is null for binary
 * files (the message then carries a placeholder instead of the content). */
export interface ContextItem {
  kind: 'note' | 'file';
  id: string;
  name: string;
  content: string | null;
}

export function contextKey(kind: 'note' | 'file', id: string): string {
  return `${kind}:${id}`;
}
