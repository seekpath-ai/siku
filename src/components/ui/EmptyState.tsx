import type { ReactNode } from 'react';

interface Props {
  icon: ReactNode;
  title: string;
  description?: string;
  badge?: string;
}

export function EmptyState({ icon, title, description, badge }: Props) {
  return (
    <div className="flex flex-col items-center justify-center py-20 text-text-secondary">
      <div className="mb-4 text-text-secondary/40">{icon}</div>
      <p className="text-lg font-medium mb-2">{title}</p>
      {description && <p className="text-sm mb-2">{description}</p>}
      {badge && (
        <div className="flex items-center gap-2 mt-4 px-4 py-2 bg-surface border border-surface-hover rounded-lg">
          <span className="text-sm">{badge}</span>
        </div>
      )}
    </div>
  );
}
