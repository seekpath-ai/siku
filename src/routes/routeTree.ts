import { Route as RootRoute } from './__root';
import { Route as IndexRoute } from './index';
import { Route as LibraryRoute } from './library';
import { Route as ReaderRoute } from './reader.$paperId';
import { Route as ChatRoute } from './chat';
import { Route as NotesRoute } from './notes';
import { Route as GraphRoute } from './graph';
import { Route as SettingsRoute } from './settings';
import { Route as KnowledgeRoute } from './knowledge';
import { Route as KnowledgeDomainRoute } from './knowledge.$domainId';
import { Route as ResearchRoute } from './research';
import { Route as ResearchTopicRoute } from './research.$topicId';
import { Route as FilesRoute } from './files';
import { Route as BookmarksRoute } from './bookmarks';
import { Route as TimelineRoute } from './timeline';

export const routeTree = RootRoute.addChildren([
  IndexRoute,
  LibraryRoute,
  ReaderRoute,
  ChatRoute,
  NotesRoute,
  GraphRoute,
  SettingsRoute,
  KnowledgeRoute,
  KnowledgeDomainRoute,
  ResearchRoute,
  ResearchTopicRoute,
  FilesRoute,
  BookmarksRoute,
  TimelineRoute,
]);
