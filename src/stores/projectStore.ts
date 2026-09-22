import { create } from 'zustand';
import { persist } from 'zustand/middleware';
import { projectsList, projectCreate, projectDelete, projectUpdate, projectSetArchived } from '@/lib/tauri';
import type { Project } from '@/lib/types';

export type SidebarSortBy = 'priority' | 'updated' | 'manual';

interface ProjectState {
  projects: Project[];
  /** Selected project filter for the chat list; null = all projects. */
  activeProjectId: string | null;
  loading: boolean;
  /** Chat list ordering. */
  sortBy: SidebarSortBy;
  /** Load the project list and restore the persisted active project. */
  load: () => Promise<void>;
  /** Add a project from a folder path and select it. Throws on failure. */
  addProject: (path: string, name?: string, gitInit?: boolean) => Promise<Project>;
  removeProject: (id: string) => Promise<void>;
  renameProject: (id: string, name: string) => Promise<void>;
  archiveProject: (id: string, archived: boolean) => Promise<void>;
  switchProject: (id: string | null) => void;
  setSortBy: (s: SidebarSortBy) => void;
}

export const useProjectStore = create<ProjectState>()(
  persist(
    (set, get) => ({
      projects: [],
      activeProjectId: null,
      loading: false,
      sortBy: 'priority',

      load: async () => {
        set({ loading: true });
        try {
          const list = await projectsList();
          const active = get().activeProjectId;
          // The restored selection must point at a visible (non-archived)
          // project; otherwise fall back to the first visible one.
          const visible = list.filter((p) => !p.archived);
          const nextActive =
            active && visible.some((p) => p.id === active) ? active : visible[0]?.id ?? null;
          set({ projects: list, activeProjectId: nextActive });
        } catch (err) {
          console.error('Failed to load projects:', err);
        } finally {
          set({ loading: false });
        }
      },

      addProject: async (path, name, gitInit) => {
        // Errors propagate to the caller (the new-project dialog shows the
        // real message, e.g. "无法创建目录") instead of being swallowed.
        const created = await projectCreate({ path, name, gitInit });
        set((s) => ({ projects: [...s.projects, created], activeProjectId: created.id }));
        return created;
      },

      removeProject: async (id) => {
        try {
          await projectDelete(id);
          const remaining = get().projects.filter((p) => p.id !== id);
          set({
            projects: remaining,
            activeProjectId:
              get().activeProjectId === id ? (remaining.find((p) => !p.archived)?.id ?? null) : get().activeProjectId,
          });
        } catch (err) {
          console.error('Failed to delete project:', err);
        }
      },

      renameProject: async (id, name) => {
        const updated = await projectUpdate(id, { name });
        set((s) => ({ projects: s.projects.map((p) => (p.id === id ? updated : p)) }));
      },

      archiveProject: async (id, archived) => {
        await projectSetArchived(id, archived);
        set((s) => ({
          projects: s.projects.map((p) => (p.id === id ? { ...p, archived } : p)),
          // Archiving the selected project clears the filter.
          activeProjectId: archived && s.activeProjectId === id ? null : s.activeProjectId,
        }));
      },

      switchProject: (id) => set({ activeProjectId: id }),
      setSortBy: (sortBy) => set({ sortBy }),
    }),
    {
      name: 'siku.chatSidebar',
      partialize: (s) => ({
        activeProjectId: s.activeProjectId,
        sortBy: s.sortBy,
      }),
    }
  )
);
