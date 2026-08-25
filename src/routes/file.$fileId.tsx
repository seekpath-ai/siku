import { createRoute, useRouter } from '@tanstack/react-router';
import { Route as RootRoute } from './__root';
import { FilePreview } from '@/components/files/FilePreview';

/** Full-page file preview route (opened as a tab on double-click). */
function FileViewPage() {
  const { fileId } = Route.useParams();
  const router = useRouter();
  return <FilePreview fileId={fileId} onBack={() => router.history.back()} />;
}

export const Route = createRoute({
  getParentRoute: () => RootRoute,
  path: '/file/$fileId',
  component: FileViewPage,
});
