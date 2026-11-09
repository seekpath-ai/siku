import { createRoute, lazyRouteComponent } from '@tanstack/react-router';
import { Route as RootRoute } from './__root';

export const Route = createRoute({
  getParentRoute: () => RootRoute,
  path: '/notes',
  validateSearch: (search: Record<string, unknown>) => ({
    note: typeof search.note === 'string' ? search.note : undefined,
  }),
  component: lazyRouteComponent(() => import('./notes.page'), 'NotesPage'),
});
