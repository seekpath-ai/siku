import { createRoute, useNavigate } from '@tanstack/react-router';
import { Route as RootRoute } from './__root';
import { useKnowledge } from '@/hooks/useKnowledge';
import { DomainGrid } from '@/components/knowledge/DomainGrid';
import { Library } from 'lucide-react';

function KnowledgePage() {
  const navigate = useNavigate();
  const { domains, activeDomainId, setActiveDomain } = useKnowledge();

  const handleSelect = (id: string) => {
    setActiveDomain(id);
    navigate({ to: `/knowledge/${id}` });
  };

  return (
    <div className="max-w-4xl mx-auto px-6 py-8 space-y-6">
      <div className="flex items-center gap-3">
        <Library size={24} className="text-primary" />
        <h1 className="text-xl font-semibold text-text-primary">知识库</h1>
      </div>
      <p className="text-sm text-text-secondary">
        五大知识域：学术研究、学习提升、生活记录、阅读笔记、个人笔记
      </p>

      <DomainGrid
        domains={domains}
        activeDomainId={activeDomainId}
        onSelect={handleSelect}
      />
    </div>
  );
}

export const Route = createRoute({
  getParentRoute: () => RootRoute,
  path: '/knowledge',
  component: KnowledgePage,
});
