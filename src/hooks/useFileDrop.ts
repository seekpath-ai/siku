import { useEffect, useRef, useState } from 'react';
import { getCurrentWindow } from '@tauri-apps/api/window';

/** OS-level file drag & drop via Tauri's native event (dragDropEnabled is on
 * by default, which suppresses HTML5 drop events on Windows — so this is the
 * only reliable channel, and it hands us real file paths instead of File
 * objects, letting drops reuse the same path-based pipeline as the picker).
 *
 * Listening is window-wide: wherever in the window files are released, the
 * drop fires. Mount once per window that accepts files. */
export function useFileDrop(onDropPaths: (paths: string[]) => void | Promise<void>) {
  const [isDragging, setIsDragging] = useState(false);
  const cbRef = useRef(onDropPaths);
  cbRef.current = onDropPaths;

  useEffect(() => {
    const unlistenPromise = getCurrentWindow().onDragDropEvent((event) => {
      const payload = event.payload;
      if (payload.type === 'enter' || payload.type === 'over') {
        setIsDragging(true);
      } else if (payload.type === 'leave') {
        setIsDragging(false);
      } else if (payload.type === 'drop') {
        setIsDragging(false);
        if (payload.paths.length > 0) void cbRef.current(payload.paths);
      }
    });
    return () => {
      unlistenPromise.then((unlisten) => unlisten()).catch(() => {});
    };
  }, []);

  return { isDragging };
}
