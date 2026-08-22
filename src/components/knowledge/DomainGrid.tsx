import type { KnowledgeDomain } from '@/lib/types';
import { DomainCard } from './DomainCard';

interface Props {
  domains: KnowledgeDomain[];
  activeDomainId: string | null;
  onSelect: (id: string) => void;
}

export function DomainGrid({ domains, activeDomainId, onSelect }: Props) {
  return (
    <div className="grid grid-cols-5 gap-4">
      {domains.map((domain) => (
        <DomainCard
          key={domain.id}
          domain={domain}
          isActive={activeDomainId === domain.id}
          onClick={() => onSelect(domain.id)}
        />
      ))}
    </div>
  );
}
