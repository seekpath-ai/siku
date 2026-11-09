import { createRoute, lazyRouteComponent } from '@tanstack/react-router';
import { Route as RootRoute } from './__root';

export const Route = createRoute({
  getParentRoute: () => RootRoute,
  path: '/reader/$paperId',
  component: lazyRouteComponent(() => import('./reader.$paperId.page'), 'ReaderPage'),
});
