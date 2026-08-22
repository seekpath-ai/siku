import { useEffect } from 'react';
import { createRoute, Link } from '@tanstack/react-router';
import { Route as RootRoute } from './__root';
import { useKnowledge } from '@/hooks/useKnowledge';
import { usePetContextStore } from '@/stores/petContextStore';
import { KnowledgeItemList } from '@/components/knowledge/KnowledgeItemList';
import { ArrowLeft } from 'lucide-react';

function KnowledgeDomainPage() {
  const { domainId } = Route.useParams();
  const {
    domains, items, isLoading, setActiveDomain, setSearchQuery,
    createItem, updateItem, deleteItem, page, hasMore, nextPage, prevPage,
  } = useKnowledge();

  useEffect(() => {
    setActiveDomain(domainId);
  }, [domainId]);

  const domain = domains.find((d) => d.id === domainId);

  // Expose the focused domain to the global pet.
  useEffect(() => {
    if (domain) {
      usePetContextStore.getState().setContext({
        page: 'knowledge',
        objectId: domain.id,
        title: domain.name || '知识库',
      });
    } else {
      usePetContextStore.getState().setContext(null);
    }
    return () => usePetContextStore.getState().setContext(null);
  }, [domain]);

  if (!domain) {
    return (
      <div className="p-8 text-text-secondary">
        <Link to="/knowledge" className="flex items-center gap-2 text-sm hover:text-text-primary mb-4">
          <ArrowLeft size={16} />返回知识库
        </Link>
        域未找到
      </div>
    );
  }

  const handleCreate = async (title: string, content: string) => {
    return createItem(domainId, title, content);
  };

  const handleUpdate = async (id: string, title: string, content: string) => {
    await updateItem(id, title, content);
  };

  return (
    <div className="max-w-3xl mx-auto px-6 py-8">
      <Link to="/knowledge" className="flex items-center gap-2 text-sm text-text-secondary hover:text-text-primary mb-6">
        <ArrowLeft size={16} />返回知识库
      </Link>

      <KnowledgeItemList
        items={items}
        isLoading={isLoading}
        domainName={domain.name}
        onCreateItem={handleCreate}
        onUpdateItem={handleUpdate}
        onDeleteItem={deleteItem}
        onSearch={setSearchQuery}
        page={page}
        hasMore={hasMore}
        onPageChange={(p) => p > page ? nextPage() : prevPage()}
      />
    </div>
  );
}

export const Route = createRoute({
  getParentRoute: () => RootRoute,
  path: '/knowledge/$domainId',
  component: KnowledgeDomainPage,
});
