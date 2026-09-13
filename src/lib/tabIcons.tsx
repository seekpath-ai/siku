import type { ReactNode } from 'react';
import {
  Bookmark,
  BookOpen,
  Bot,
  Clock,
  FileText,
  FileType2,
  FlaskConical,
  Folder,
  FolderOpen,
  GitGraph,
  Home,
  Library,
  Settings,
  Star,
  StickyNote,
} from 'lucide-react';
import { HOME_TAB_ID, type Tab } from '@/stores/tabStore';

/** The sidebar's routes and their icons — the single source shared with the tab
 *  strip, so a tab can never show a different icon than the sidebar entry that
 *  opened it. (`/library` is not a house: the house belongs to the home tab,
 *  whichever route the user pinned as home.) */
const NAV_ICONS: Record<string, typeof Library> = {
  '/library': Library,
  '/chat': Bot,
  '/notes': StickyNote,
  '/knowledge': FolderOpen,
  '/research': FlaskConical,
  '/graph': GitGraph,
  '/bookmarks': Bookmark,
  '/timeline': Clock,
  '/files': Folder,
  '/settings': Settings,
};

/** Icon for a sidebar route, or null for a route the sidebar doesn't list. */
export function navIcon(route: string, size = 18): ReactNode {
  const Icon = NAV_ICONS[route];
  return Icon ? <Icon size={size} /> : null;
}

/** Icon for one tab: the home tab gets the house, every other tab mirrors its
 *  sidebar entry, and tabs for things the sidebar doesn't list (papers, notes,
 *  searches) fall back to their own kind. */
export function tabIconNode(tab: Pick<Tab, 'id' | 'route' | 'icon'>, size = 13): ReactNode {
  if (tab.id === HOME_TAB_ID) return <Home size={size} />;
  const fromSidebar = navIcon(tab.route, size);
  if (fromSidebar) return fromSidebar;

  const common = { size } as const;
  switch (tab.icon) {
    case 'note': return <FileText {...common} />;
    case 'paper':
    case 'pdf': return <BookOpen {...common} />;
    case 'star': return <Star {...common} />;
    default: return <FileType2 {...common} />;
  }
}
