import { useCallback, useState } from 'react';
import { readDocumentFile } from '@/lib/tauri';

/** Document attachment staged in an input box: the file is extracted to text
 * at attach time and injected into the next message as a <file> block. */
export interface TextAttachment {
  kind: 'text';
  id: string;
  path: string;
  name: string;
  content: string;
}

/** Shared text-attachment staging for message inputs (main chat + pet
 * conversation), mirroring useImageAttachments for documents. */
export function useTextAttachments() {
  const [attachments, setAttachments] = useState<TextAttachment[]>([]);

  /** Extract one document path and stage it (dedupes by path). Returns an
   * error string on failure, null on success. cacheDir is the agent session
   * write dir when known, so oversized extractions can be paged later. */
  const addPath = useCallback(async (path: string, cacheDir?: string): Promise<string | null> => {
    const name = path.split(/[\\/]/).pop() || path;
    try {
      const content = await readDocumentFile(path, cacheDir);
      setAttachments((prev) =>
        prev.some((a) => a.path === path)
          ? prev
          : [...prev, { kind: 'text', id: `txt_${Date.now()}_${path.length}`, path, name, content }]
      );
      return null;
    } catch (err) {
      return `${name}: ${err}`;
    }
  }, []);

  const removeAttachment = useCallback((id: string) => {
    setAttachments((prev) => prev.filter((a) => a.id !== id));
  }, []);

  const clearAttachments = useCallback(() => setAttachments([]), []);

  /** Render staged attachments as the <file> block prepended to a message. */
  const toMessageBlock = useCallback(
    () =>
      attachments
        .map((a) => `<file name="${a.name}" path="${a.path}">\n${a.content}\n</file>`)
        .join('\n'),
    [attachments]
  );

  return { attachments, addPath, removeAttachment, clearAttachments, toMessageBlock };
}
