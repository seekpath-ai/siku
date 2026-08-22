import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query';
import * as tauri from '@/lib/tauri';
import type { ListPapersParams, Paper, PaperInput, PaperLinkMetadata, Collection, Tag, Note } from '@/lib/types';

/** Fetch papers with caching */
export function usePapers(params?: ListPapersParams) {
  return useQuery({
    queryKey: ['papers', params],
    queryFn: () => tauri.listPapers(params),
    staleTime: 30_000,
  });
}

/** Fetch a single paper by ID */
export function usePaper(id: string | null) {
  return useQuery({
    queryKey: ['paper', id],
    queryFn: () => tauri.getPaper(id!),
    enabled: !!id,
    staleTime: 30_000,
  });
}

/** Fetch notes linked to a paper */
export function usePaperNotes(paperId: string | null) {
  return useQuery<Note[]>({
    queryKey: ['paper-notes', paperId],
    queryFn: () => tauri.notesList(paperId!),
    enabled: !!paperId,
    staleTime: 30_000,
  });
}

/** Import a PDF file */
export function useImportPaper() {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: (filePath: string) => tauri.importPaper(filePath),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['papers'] });
    },
    onError: (err: unknown) => {
      const msg = err instanceof Error ? err.message : String(err);
      console.error('PDF import failed:', msg);
    },
  });
}

/** Import a paper from a URL (DOI, arXiv, PubMed, Semantic Scholar, direct PDF). */
export function useImportPaperFromLink() {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: (input: { url: string; metadata?: PaperLinkMetadata }) =>
      tauri.importPaperFromLink(input.url, input.metadata),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['papers'] });
    },
    onError: (err: unknown) => {
      const parsed = tauri.parseLinkImportError(err);
      console.error('Link import failed:', parsed);
    },
  });
}

/** Preview metadata for a link without importing. */
export function usePreviewPaperFromLink() {
  return useMutation({
    mutationFn: (url: string) => tauri.previewPaperFromLink(url),
  });
}

/** Delete a paper (moves it to trash) */
export function useDeletePaper() {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: (id: string) => tauri.deletePaper(id),
    onSuccess: (_data, id) => {
      queryClient.invalidateQueries({ queryKey: ['papers'] });
      queryClient.removeQueries({ queryKey: ['paper', id] });
    },
    onError: (err: unknown) => {
      const msg = err instanceof Error ? err.message : String(err);
      alert(`删除文献失败: ${msg}`);
    },
  });
}

/** Restore a trashed paper */
export function useRestorePaper() {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: (id: string) => tauri.paperRestore(id),
    onSuccess: (_data, id) => {
      queryClient.invalidateQueries({ queryKey: ['papers'] });
      queryClient.removeQueries({ queryKey: ['paper', id] });
    },
    onError: (err: unknown) => {
      const msg = err instanceof Error ? err.message : String(err);
      alert(`恢复文献失败: ${msg}`);
    },
  });
}

/** Permanently delete a trashed paper */
export function usePurgePaper() {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: (id: string) => tauri.paperPurge(id),
    onSuccess: (_data, id) => {
      queryClient.invalidateQueries({ queryKey: ['papers'] });
      queryClient.removeQueries({ queryKey: ['paper', id] });
    },
    onError: (err: unknown) => {
      const msg = err instanceof Error ? err.message : String(err);
      alert(`永久删除失败: ${msg}`);
    },
  });
}

/** Toggle a paper's favorite flag (optimistic). */
export function usePaperSetFavorite() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: ({ id, favorite }: { id: string; favorite: boolean }) =>
      tauri.paperSetFavorite(id, favorite),
    onMutate: ({ id, favorite }) => {
      queryClient.setQueriesData<Paper[]>({ queryKey: ['papers'] }, (old) =>
        old?.map((p) => (p.id === id ? { ...p, is_favorite: favorite ? 1 : 0 } : p))
      );
      queryClient.setQueryData<Paper>(['paper', id], (old) =>
        old ? { ...old, is_favorite: favorite ? 1 : 0 } : old
      );
    },
    onError: (err: unknown) => {
      const msg = err instanceof Error ? err.message : String(err);
      alert(`星标操作失败: ${msg}`);
      queryClient.invalidateQueries({ queryKey: ['papers'] });
    },
  });
}

/** Set a paper's read status (optimistic). */
export function usePaperSetReadStatus() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: ({ id, status }: { id: string; status: string }) =>
      tauri.paperSetReadStatus(id, status),
    onMutate: ({ id, status }) => {
      queryClient.setQueriesData<Paper[]>({ queryKey: ['papers'] }, (old) =>
        old?.map((p) => (p.id === id ? { ...p, read_status: status } : p))
      );
      queryClient.setQueryData<Paper>(['paper', id], (old) =>
        old ? { ...old, read_status: status } : old
      );
    },
    onError: (err: unknown) => {
      const msg = err instanceof Error ? err.message : String(err);
      alert(`更新阅读状态失败: ${msg}`);
      queryClient.invalidateQueries({ queryKey: ['papers'] });
    },
  });
}

/** Update paper metadata */
export function useUpdatePaper() {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: ({ id, input }: { id: string; input: PaperInput }) =>
      tauri.updatePaper(id, input),
    onSuccess: (data) => {
      queryClient.invalidateQueries({ queryKey: ['papers'] });
      queryClient.invalidateQueries({ queryKey: ['paper', data.id] });
    },
  });
}

// ============================================================
// Collections
// ============================================================

export function useCollections() {
  return useQuery<Collection[]>({
    queryKey: ['collections'],
    queryFn: () => tauri.collectionsList(),
    staleTime: 60_000,
  });
}

export function useCreateCollection() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: ({ name, parentId }: { name: string; parentId?: string }) =>
      tauri.collectionsCreate(name, parentId),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['collections'] });
    },
  });
}

export function useUpdateCollection() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: ({ id, name, parentId }: { id: string; name?: string; parentId?: string | null }) =>
      tauri.collectionsUpdate(id, name, parentId),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['collections'] });
    },
  });
}

export function useDeleteCollection() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (id: string) => tauri.collectionsDelete(id),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['collections'] });
      queryClient.invalidateQueries({ queryKey: ['papers'] });
    },
  });
}

export function useAddPapersToCollection() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: ({ collectionId, paperIds }: { collectionId: string; paperIds: string[] }) =>
      tauri.collectionsAddPapers(collectionId, paperIds),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['papers'] });
      queryClient.invalidateQueries({ queryKey: ['collections'] });
    },
  });
}

export function useRemovePapersFromCollection() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: ({ collectionId, paperIds }: { collectionId: string; paperIds: string[] }) =>
      tauri.collectionsRemovePapers(collectionId, paperIds),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['papers'] });
      queryClient.invalidateQueries({ queryKey: ['collections'] });
    },
  });
}

// ============================================================
// Tags
// ============================================================

export function useTags() {
  return useQuery<Tag[]>({
    queryKey: ['tags'],
    queryFn: () => tauri.tagsList(),
    staleTime: 60_000,
  });
}

export function useCreateTag() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: ({ name, color }: { name: string; color?: string }) =>
      tauri.tagsCreate(name, color),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['tags'] });
    },
  });
}

export function useDeleteTag() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (id: string) => tauri.tagsDelete(id),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['tags'] });
      queryClient.invalidateQueries({ queryKey: ['papers'] });
    },
  });
}

export function useUpdateTag() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: ({ id, name, color }: { id: string; name?: string; color?: string }) =>
      tauri.tagsUpdate(id, name, color),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['tags'] });
    },
  });
}

export function usePaperTags(paperId: string | null) {
  return useQuery<Tag[]>({
    queryKey: ['paper-tags', paperId],
    queryFn: () => tauri.tagsPapers(paperId!),
    enabled: !!paperId,
    staleTime: 30_000,
  });
}

export function useAddTagsToPaper() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: ({ paperId, tagIds }: { paperId: string; tagIds: string[] }) =>
      tauri.tagsAddToPaper(paperId, tagIds),
    onSuccess: (_data, vars) => {
      queryClient.invalidateQueries({ queryKey: ['paper-tags', vars.paperId] });
      queryClient.invalidateQueries({ queryKey: ['tags'] });
      queryClient.invalidateQueries({ queryKey: ['papers'] });
    },
  });
}

export function useRemoveTagsFromPaper() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: ({ paperId, tagIds }: { paperId: string; tagIds: string[] }) =>
      tauri.tagsRemoveFromPaper(paperId, tagIds),
    onSuccess: (_data, vars) => {
      queryClient.invalidateQueries({ queryKey: ['paper-tags', vars.paperId] });
      queryClient.invalidateQueries({ queryKey: ['tags'] });
      queryClient.invalidateQueries({ queryKey: ['papers'] });
    },
  });
}
