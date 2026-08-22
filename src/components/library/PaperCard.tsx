import { FileText, Calendar, User, BookOpen } from 'lucide-react';
import { isoToDisplay } from '@/lib/time';
import { parseJsonArray } from '@/lib/types';
import type { Paper } from '@/lib/types';

interface PaperCardProps {
  paper: Paper;
  isSelected: boolean;
  onClick: () => void;
}

export function PaperCard({ paper, isSelected, onClick }: PaperCardProps) {
  const authors = parseJsonArray(paper.authors);
  const displayAuthors =
    authors.length > 0 ? authors.slice(0, 3).join(', ') + (authors.length > 3 ? ' 等' : '') : null;

  return (
    <div
      onClick={onClick}
      className={`p-4 rounded-lg border cursor-pointer transition-all ${
        isSelected
          ? 'border-primary bg-primary/5 shadow-sm'
          : 'border-surface-hover bg-surface hover:border-text-secondary/30 hover:bg-surface-hover'
      }`}
    >
      {/* Title */}
      <h3 className="text-sm font-medium text-text-primary line-clamp-2 mb-2 leading-snug">
        {paper.title || '未命名文献'}
      </h3>

      {/* Authors */}
      {displayAuthors && (
        <div className="flex items-center gap-1.5 text-xs text-text-secondary mb-1.5">
          <User size={12} />
          <span className="line-clamp-1">{displayAuthors}</span>
        </div>
      )}

      {/* Meta row */}
      <div className="flex items-center gap-3 text-xs text-text-secondary flex-wrap">
        {paper.year && (
          <span className="flex items-center gap-1">
            <Calendar size={12} />
            {paper.year}
          </span>
        )}
        {paper.journal && (
          <span className="flex items-center gap-1 line-clamp-1">
            <BookOpen size={12} />
            {paper.journal}
          </span>
        )}
        {paper.page_count != null && paper.page_count > 0 && (
          <span className="flex items-center gap-1">
            <FileText size={12} />
            {paper.page_count} 页
          </span>
        )}
      </div>

      {/* Import date */}
      <div className="mt-2.5 text-[11px] text-text-secondary/60">
        {isoToDisplay(paper.imported_at)}
      </div>
    </div>
  );
}
