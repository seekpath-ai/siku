import { useEffect, useState } from 'react';
import { ChevronRight, ChevronDown } from 'lucide-react';
import type { PDFDocumentProxy } from 'pdfjs-dist';

interface OutlineItem {
  title: string;
  bold: boolean;
  italic: boolean;
  color: Uint8ClampedArray;
  dest: string | Array<unknown> | null;
  url: string | null;
  unsafeUrl: string | undefined;
  newWindow: boolean | undefined;
  count: number | undefined;
  items: OutlineItem[];
}

interface PdfOutlineProps {
  doc: PDFDocumentProxy;
  onJumpToPage: (page: number) => void;
}

async function resolveOutlinePage(doc: PDFDocumentProxy, dest: string | Array<unknown> | null): Promise<number | null> {
  if (!dest) return null;
  try {
    let explicit: Array<unknown> | null;
    if (typeof dest === 'string') {
      explicit = await doc.getDestination(dest);
    } else {
      explicit = dest;
    }
    if (!Array.isArray(explicit) || explicit.length === 0) return null;
    const ref = explicit[0] as Parameters<PDFDocumentProxy['getPageIndex']>[0];
    const idx = await doc.getPageIndex(ref);
    return typeof idx === 'number' ? idx + 1 : null;
  } catch {
    return null;
  }
}

function OutlineNode({
  item,
  level,
  onClick,
}: {
  item: OutlineItem;
  level: number;
  onClick: (item: OutlineItem) => void;
}) {
  const [expanded, setExpanded] = useState(true);
  const hasChildren = item.items && item.items.length > 0;
  return (
    <div className="select-none">
      <button
        onClick={() => {
          if (hasChildren) setExpanded((v) => !v);
          onClick(item);
        }}
        className="flex w-full items-center gap-1 rounded px-1.5 py-1 text-left text-xs text-text-secondary hover:bg-surface-hover hover:text-text-primary"
        style={{ paddingLeft: `${8 + level * 12}px` }}
      >
        <span className="w-3.5 shrink-0">
          {hasChildren ? (
            expanded ? <ChevronDown size={12} /> : <ChevronRight size={12} />
          ) : null}
        </span>
        <span
          className="truncate"
          style={{
            fontWeight: item.bold ? 600 : 400,
            fontStyle: item.italic ? 'italic' : 'normal',
          }}
        >
          {item.title || '（无标题）'}
        </span>
      </button>
      {hasChildren && expanded && (
        <div>
          {item.items.map((child, i) => (
            <OutlineNode key={i} item={child} level={level + 1} onClick={onClick} />
          ))}
        </div>
      )}
    </div>
  );
}

export function PdfOutline({ doc, onJumpToPage }: PdfOutlineProps) {
  const [outline, setOutline] = useState<OutlineItem[] | null>(null);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    doc.getOutline().then((items) => {
      if (!cancelled) {
        setOutline((items as OutlineItem[] | null) || []);
        setLoading(false);
      }
    }).catch(() => {
      if (!cancelled) {
        setOutline([]);
        setLoading(false);
      }
    });
    return () => { cancelled = true; };
  }, [doc]);

  const handleClick = async (item: OutlineItem) => {
    const page = await resolveOutlinePage(doc, item.dest);
    if (page) onJumpToPage(page);
  };

  if (loading) {
    return (
      <div className="flex h-full items-center justify-center text-xs text-text-secondary">
        加载目录...
      </div>
    );
  }

  if (!outline || outline.length === 0) {
    return (
      <div className="flex h-full items-center justify-center text-xs text-text-secondary">
        无目录
      </div>
    );
  }

  return (
    <div className="h-full overflow-auto py-2">
      {outline.map((item, i) => (
        <OutlineNode key={i} item={item} level={0} onClick={handleClick} />
      ))}
    </div>
  );
}
