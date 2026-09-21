import { create } from 'zustand';
import { listen } from '@tauri-apps/api/event';
import {
  BATCH_IMPORT_PROGRESS_EVENT,
  importPapersBatch,
  libraryCancelBatchImport,
  libraryScanFolder,
  zoteroImport,
  type BatchImportProgress,
} from '@/lib/tauri';

/** Progress/state for the library batch-import dialog (multi-select files,
 *  folder scan, Zotero). The backend emits one progress event per item and a
 *  terminal `done` event; this store just mirrors the latest payload so the
 *  dialog can live anywhere. */

interface BatchImportState {
  open: boolean;
  running: boolean;
  /** Dialog heading, e.g. "批量导入" / "从 Zotero 导入". */
  label: string;
  total: number;
  current: number;
  file: string;
  imported: number;
  skipped: number;
  failed: number;
  cancelled: boolean;
  done: boolean;
  errors: string[];
  /** Start a batch: open the dialog, run the backend call, keep the summary. */
  run: (label: string, total: number, task: () => Promise<{ errors: string[] }>) => Promise<void>;
  cancel: () => void;
  close: () => void;
  /** Pick a folder, scan it for PDFs and import them all. Returns false when
   *  the user aborted or the folder held no PDFs (caller may toast). */
  runFolder: (dirPath: string) => Promise<number>;
}

let listenerReady = false;

function ensureListener() {
  if (listenerReady) return;
  listenerReady = true;
  listen<BatchImportProgress>(BATCH_IMPORT_PROGRESS_EVENT, (e) => {
    const p = e.payload;
    useBatchImportStore.setState((s) => ({
      total: p.total,
      current: p.current,
      file: p.file,
      imported: p.imported,
      skipped: p.skipped,
      failed: p.failed,
      cancelled: s.cancelled || p.cancelled,
      done: s.done || p.done,
      running: s.running && !p.done,
    }));
  }).catch(() => {});
}

export const useBatchImportStore = create<BatchImportState>((set, get) => ({
  open: false,
  running: false,
  label: '批量导入',
  total: 0,
  current: 0,
  file: '',
  imported: 0,
  skipped: 0,
  failed: 0,
  cancelled: false,
  done: false,
  errors: [],

  run: async (label, total, task) => {
    if (get().running) return;
    ensureListener();
    set({
      open: true, running: true, label, total,
      current: 0, file: '', imported: 0, skipped: 0, failed: 0,
      cancelled: false, done: false, errors: [],
    });
    try {
      const summary = await task();
      set({ running: false, done: true, errors: summary.errors });
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      set({ running: false, done: true, errors: [msg] });
    }
  },

  runFolder: async (dirPath) => {
    const paths = await libraryScanFolder(dirPath, true);
    if (paths.length === 0) return 0;
    await get().run('从文件夹导入', paths.length, () => importPapersBatch(paths, 'folder'));
    return paths.length;
  },

  cancel: () => {
    libraryCancelBatchImport().catch(() => {});
  },

  close: () => {
    if (get().running) return; // running batches must be cancelled first
    set({ open: false });
  },
}));

/** Entry point shared by the multi-select file dialog and drag-and-drop. */
export async function runFileBatch(paths: string[]): Promise<void> {
  await useBatchImportStore
    .getState()
    .run('批量导入', paths.length, () => importPapersBatch(paths, 'files'));
}

/** Entry point for the Zotero wizard. */
export async function runZoteroImport(total: number, path?: string): Promise<void> {
  await useBatchImportStore
    .getState()
    .run('从 Zotero 导入', total, () => zoteroImport(path));
}
