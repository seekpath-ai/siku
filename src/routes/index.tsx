import { createRoute, redirect } from '@tanstack/react-router';
import { Route as RootRoute } from './__root';
import { settingsAppGet } from '@/lib/tauri';
import { useTabStore } from '@/stores/tabStore';

const validHomePages = new Set([
  '/library',
  '/chat',
  '/notes',
  '/knowledge',
  '/research',
  '/graph',
  '/bookmarks',
  '/timeline',
  '/files',
  '/settings',
]);

// Redirect root to the user-configured homepage (default: /library).
export const Route = createRoute({
  getParentRoute: () => RootRoute,
  path: '/',
  beforeLoad: async () => {
    let homepage = '/library';
    try {
      const settings = await settingsAppGet();
      if (settings.homepage && validHomePages.has(settings.homepage)) {
        homepage = settings.homepage;
      }
    } catch {
      // ignore and fall back to default
    }
    useTabStore.getState().setHomeTab(homepage);
    throw redirect({ to: homepage as '/' });
  },
});
