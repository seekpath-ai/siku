import { useState } from 'react';
import { FileDown, X } from 'lucide-react';

export interface PdfExportOptions {
  /** 章节自动编号：正文 h1–h4 前加 1 / 1.1 / 1.1.1 */
  numberSections: boolean;
  /** 图表自动编号：图 N（含 alt 文本）、表 N */
  numberFigures: boolean;
  /** 封面页：标题 + 导出日期独占一页 */
  coverPage: boolean;
}

const STORAGE_KEY = 'siku.pdf-export-options';

function loadOptions(): PdfExportOptions {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (raw) return { numberSections: false, numberFigures: false, coverPage: false, ...JSON.parse(raw) };
  } catch { /* corrupted value — fall through to defaults */ }
  return { numberSections: false, numberFigures: false, coverPage: false };
}

interface Props {
  noteTitle: string;
  onConfirm: (options: PdfExportOptions) => void;
  onCancel: () => void;
}

/** Pre-export options for 导出为 PDF. The choices are device-local
 *  (localStorage) — they are print-layout preferences, not note data. */
export function ExportPdfDialog({ noteTitle, onConfirm, onCancel }: Props) {
  const [options, setOptions] = useState<PdfExportOptions>(loadOptions);

  const toggle = (key: keyof PdfExportOptions) =>
    setOptions((o) => ({ ...o, [key]: !o[key] }));

  const handleConfirm = () => {
    try {
      localStorage.setItem(STORAGE_KEY, JSON.stringify(options));
    } catch { /* storage full/denied — options just won't persist */ }
    onConfirm(options);
  };

  const items: { key: keyof PdfExportOptions; label: string; hint: string }[] = [
    { key: 'numberSections', label: '章节自动编号', hint: '正文标题前加 1 / 1.1 / 1.1.1' },
    { key: 'numberFigures', label: '图表自动编号', hint: '图片下方加「图 N」，表格加「表 N」' },
    { key: 'coverPage', label: '封面页', hint: '标题与导出日期独占首页' },
  ];

  return (
    <div className="fixed inset-0 z-[200] flex items-center justify-center">
      <div className="absolute inset-0 bg-black/50 backdrop-blur-sm" onClick={onCancel} />
      <div className="relative w-[380px] max-w-[92vw] flex flex-col bg-surface border border-surface-hover rounded-xl shadow-2xl overflow-hidden">
        <div className="flex items-center justify-between px-4 py-2.5 border-b border-surface-hover shrink-0">
          <span className="text-sm font-medium text-text-primary flex items-center gap-1.5">
            <FileDown size={14} className="text-primary" />
            导出为 PDF
          </span>
          <button
            onClick={onCancel}
            className="p-1 rounded text-text-secondary/60 hover:text-text-primary hover:bg-surface-hover transition-colors"
            aria-label="关闭"
          >
            <X size={14} />
          </button>
        </div>

        <div className="px-4 py-3 flex flex-col gap-2.5">
          <div className="text-xs text-text-secondary truncate" title={noteTitle}>
            {noteTitle || '未命名笔记'}
          </div>
          {items.map((item) => (
            <label
              key={item.key}
              className="flex items-center gap-2.5 px-2.5 py-2 rounded-lg border border-surface-hover hover:bg-surface-hover/50 cursor-pointer transition-colors"
            >
              <input
                type="checkbox"
                checked={options[item.key]}
                onChange={() => toggle(item.key)}
                className="accent-primary"
              />
              <span className="flex-1 min-w-0">
                <span className="block text-xs text-text-primary">{item.label}</span>
                <span className="block text-[11px] text-text-secondary/70">{item.hint}</span>
              </span>
            </label>
          ))}
        </div>

        <div className="flex items-center justify-end gap-2 px-4 py-2.5 border-t border-surface-hover shrink-0">
          <button
            onClick={onCancel}
            className="px-3 py-1.5 rounded text-xs text-text-secondary hover:bg-surface-hover hover:text-text-primary transition-colors"
          >
            取消
          </button>
          <button
            onClick={handleConfirm}
            className="px-3 py-1.5 rounded text-xs bg-primary/15 text-primary hover:bg-primary/25 transition-colors"
          >
            导出
          </button>
        </div>
      </div>
    </div>
  );
}
