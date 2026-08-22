import { createRoute } from '@tanstack/react-router';
import { Route as RootRoute } from './__root';
import { LibraryLayout } from '@/components/library/LibraryLayout';

export const Route = createRoute({
  getParentRoute: () => RootRoute,
  path: '/library',
  component: LibraryLayout,
});
