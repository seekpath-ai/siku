import { useEffect } from 'react';
import { createPortal } from 'react-dom';
import { WikiMarkdown } from './WikiMarkdown';
import type { PdfExportOptions } from './ExportPdfDialog';
import type { Note } from '@/lib/types';

interface Props {
  note: Note;
  content: string;
  notes: Note[];
  attachmentsDir?: string;
  options: PdfExportOptions;
  /** Called after the print dialog closes (or on unmount). */
  onDone: () => void;
}

/**
 * Print-to-PDF: renders a print-only copy of the note into document.body
 * (CSS hides the app shell under @media print) and opens the system print
 * dialog — GTK on Linux and WebView2 on Windows both offer "save as PDF".
 * The portal is removed on `afterprint`.
 */
export function PrintNotePortal({ note, content, notes, attachmentsDir, options, onDone }: Props) {
  // Figure/table auto-numbering: generated content can't attach captions to
  // replaced elements (<img>) and markdown tables have no <caption>, so the
  // captions are real DOM inserted once before printing. Only block images
  // (a paragraph holding just the image) get 图 N — inline icons don't.
  useEffect(() => {
    if (!options.numberFigures) return;
    const root = document.getElementById('siku-print-root');
    if (!root) return;
    let fig = 0;
    root.querySelectorAll('p > img:only-child').forEach((img) => {
      fig += 1;
      const alt = (img.getAttribute('alt') || '').trim();
      const cap = document.createElement('div');
      cap.className = 'pdf-figcap';
      cap.textContent = '图 ' + fig + (alt ? '　' + alt : '');
      img.parentElement?.insertAdjacentElement('afterend', cap);
    });
    let tbl = 0;
    root.querySelectorAll('table').forEach((table) => {
      tbl += 1;
      const cap = document.createElement('caption');
      cap.className = 'pdf-tblcap';
      cap.textContent = `表 ${tbl}`;
      table.prepend(cap);
    });
  }, [options.numberFigures]);

  useEffect(() => {
    let cancelled = false;
    // The system "save as PDF" dialog suggests document.title as the file
    // name — temporarily swap in the note title, restore after printing.
    const prevTitle = document.title;
    document.title = note.title || '未命名笔记';
    const finish = () => {
      document.title = prevTitle;
      if (!cancelled) onDone();
    };
    window.addEventListener('afterprint', finish);

    // Wait for images to finish loading before opening the dialog, otherwise
    // the PDF can come out with missing figures. A hard timeout keeps a stuck
    // image from blocking the dialog forever.
    const root = document.getElementById('siku-print-root');
    const pending = Array.from(root?.querySelectorAll('img') ?? []).filter((img) => !img.complete);
    let remaining = pending.length;
    let printed = false;
    const doPrint = () => {
      if (printed || cancelled) return;
      printed = true;
      window.print();
      // Safety net: if afterprint never fires (e.g. dialog suppressed),
      // still clean up so the app isn't stuck in print mode.
      setTimeout(finish, 60_000);
    };
    const onImgSettled = () => {
      if (--remaining <= 0) doPrint();
    };
    let timer: ReturnType<typeof setTimeout> | undefined;
    if (pending.length === 0) {
      // Let the layout settle for a frame so KaTeX/tables are measured.
      requestAnimationFrame(() => requestAnimationFrame(doPrint));
    } else {
      pending.forEach((img) => {
        img.addEventListener('load', onImgSettled, { once: true });
        img.addEventListener('error', onImgSettled, { once: true });
      });
      timer = setTimeout(doPrint, 3000);
    }

    return () => {
      cancelled = true;
      if (timer) clearTimeout(timer);
      document.title = prevTitle;
      window.removeEventListener('afterprint', finish);
    };
  }, [note.title, onDone]);

  const rootClass = [
    options.numberSections ? 'pdf-number-sections' : '',
    options.numberFigures ? 'pdf-number-figures' : '',
  ]
    .filter(Boolean)
    .join(' ');

  return createPortal(
    <div id="siku-print-root" className={rootClass || undefined}>
      {options.coverPage && (
        <div className="pdf-cover">
          <h1 className="pdf-cover-title">{note.title || '未命名笔记'}</h1>
          <div className="pdf-cover-date">
            {new Date().toLocaleDateString('zh-CN', {
              year: 'numeric',
              month: 'long',
              day: 'numeric',
            })}
          </div>
        </div>
      )}
      {!options.coverPage && <h1 className="siku-print-title">{note.title || '未命名笔记'}</h1>}
      <WikiMarkdown
        content={content || ' '}
        notes={notes}
        onNavigate={() => {}}
        attachmentsDir={attachmentsDir}
        className="prose max-w-none"
      />
    </div>,
    document.body
  );
}
