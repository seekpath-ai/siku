import { GraduationCap, BookOpen, Heart, Bookmark, StickyNote } from 'lucide-react';
import type { KnowledgeDomain } from '@/lib/types';

const iconMap: Record<string, React.ReactNode> = {
  'graduation-cap': <GraduationCap size={28} />,
  'book-open': <BookOpen size={28} />,
  heart: <Heart size={28} />,
  bookmark: <Bookmark size={28} />,
  'sticky-note': <StickyNote size={28} />,
};

interface Props {
  domain: KnowledgeDomain;
  isActive: boolean;
  onClick: () => void;
}

export function DomainCard({ domain, isActive, onClick }: Props) {
  return (
    <button
      onClick={onClick}
      className={`flex flex-col items-center gap-3 p-6 rounded-xl border transition-all ${
        isActive
          ? 'border-primary bg-primary/5 shadow-lg shadow-primary/10'
          : 'border-surface-hover bg-surface hover:bg-surface-hover'
      }`}
    >
      <div
        className="w-14 h-14 rounded-xl flex items-center justify-center"
        style={{ backgroundColor: `${domain.color}20`, color: domain.color }}
      >
        {iconMap[domain.icon || ''] || <StickyNote size={28} />}
      </div>
      <span className="text-sm font-medium text-text-primary">{domain.name}</span>
    </button>
  );
}
