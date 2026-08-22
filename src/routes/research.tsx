import { useState } from 'react';
import { createRoute, useNavigate } from '@tanstack/react-router';
import { Route as RootRoute } from './__root';
import { useResearch } from '@/hooks/useResearch';
import { TopicCard } from '@/components/research/TopicCard';
import { TopicForm } from '@/components/research/TopicForm';
import { FlaskConical, Plus } from 'lucide-react';

function ResearchPage() {
  const navigate = useNavigate();
  const { topics, activeTopicId, setActiveTopic, isDiscovering, createTopic, discoverSources, updateTopic, deleteTopic } = useResearch();
  const [showForm, setShowForm] = useState(false);

  const handleCreate = async (name: string, keywords: string[], description?: string) => {
    await createTopic(name, keywords, description);
    setShowForm(false);
  };

  const handleSelect = (id: string) => {
    setActiveTopic(id);
    navigate({ to: `/research/${id}` });
  };

  const activeTopics = topics.filter((t) => t.status !== 'archived');
  const archivedTopics = topics.filter((t) => t.status === 'archived');

  return (
    <div className="max-w-4xl mx-auto px-6 py-8 space-y-6">
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-3">
          <FlaskConical size={24} className="text-primary" />
          <h1 className="text-xl font-semibold text-text-primary">科研追踪</h1>
        </div>
        <button
          onClick={() => setShowForm(!showForm)}
          className="flex items-center gap-1 px-3 py-1.5 rounded-lg bg-primary/10 text-primary text-sm hover:bg-primary/20"
        >
          <Plus size={14} />新建课题
        </button>
      </div>

      {showForm && <TopicForm onCreate={handleCreate} onCancel={() => setShowForm(false)} />}

      <div className="grid grid-cols-2 gap-4">
        {activeTopics.map((t) => (
          <TopicCard
            key={t.id}
            topic={t}
            isActive={activeTopicId === t.id}
            onSelect={() => handleSelect(t.id)}
            onDiscover={() => discoverSources(t.id)}
            onTogglePause={() => updateTopic(t.id, t.status === 'active' ? 'paused' : 'active')}
            onArchive={() => updateTopic(t.id, 'archived')}
            onDelete={() => deleteTopic(t.id)}
            isDiscovering={isDiscovering}
          />
        ))}
      </div>

      {activeTopics.length === 0 && !showForm && (
        <p className="text-center text-text-secondary py-8 text-sm">暂无活跃课题。创建一个开始追踪前沿研究。</p>
      )}

      {archivedTopics.length > 0 && (
        <>
          <h2 className="text-sm font-medium text-text-secondary mt-8">已归档</h2>
          <div className="grid grid-cols-2 gap-4 opacity-60">
            {archivedTopics.map((t) => (
              <TopicCard
                key={t.id}
                topic={t}
                isActive={false}
                onSelect={() => handleSelect(t.id)}
                onDiscover={() => {}}
                onTogglePause={() => updateTopic(t.id, 'active')}
                onArchive={() => {}}
                onDelete={() => deleteTopic(t.id)}
                isDiscovering={false}
              />
            ))}
          </div>
        </>
      )}
    </div>
  );
}

export const Route = createRoute({
  getParentRoute: () => RootRoute,
  path: '/research',
  component: ResearchPage,
});
