import { create } from 'zustand';
import { persist } from 'zustand/middleware';
import { projectsList, projectCreate, projectDelete } from '@/lib/tauri';
import type { Project } from '@/lib/types';

export type SidebarGroupBy = 'project' | 'list';
export type SidebarSortBy = 'priority' | 'updated' | 'manual';

interface ProjectState {
  projects: Project[];
  /** Selected project filter for the chat list; null = all projects. */
  activeProjectId: string | null;
  loading: boolean;
  /** Chat list grouping: by project or one flat list. */
  groupBy: SidebarGroupBy;
  /** Chat list ordering. */
  sortBy: SidebarSortBy;
  /** Load the project list and restore the persisted active project. */
  load: () => Promise<void>;
  /** Add a project from a folder path and select it. */
  addProject: (path: string, name?: string) => Promise<Project | null>;
  removeProject: (id: string) => Promise<void>;
  switchProject: (id: string | null) => void;
  setGroupBy: (g: SidebarGroupBy) => void;
  setSortBy: (s: SidebarSortBy) => void;
}

export const useProjectStore = create<ProjectState>()(
  persist(
    (set, get) => ({
      projects: [],
      activeProjectId: null,
      loading: false,
      groupBy: 'project',
      sortBy: 'priority',

      load: async () => {
        set({ loading: true });
        try {
          const list = await projectsList();
          const active = get().activeProjectId;
          const nextActive =
            active && list.some((p) => p.id === active) ? active : list[0]?.id ?? null;
          set({ projects: list, activeProjectId: nextActive });
        } catch (err) {
          console.error('Failed to load projects:', err);
        } finally {
          set({ loading: false });
        }
      },

      addProject: async (path, name) => {
        try {
          const created = await projectCreate({ path, name });
          set((s) => ({ projects: [...s.projects, created], activeProjectId: created.id }));
          return created;
        } catch (err) {
          console.error('Failed to create project:', err);
          return null;
        }
      },

      removeProject: async (id) => {
        try {
          await projectDelete(id);
          const remaining = get().projects.filter((p) => p.id !== id);
          set({
            projects: remaining,
            activeProjectId:
              get().activeProjectId === id ? (remaining[0]?.id ?? null) : get().activeProjectId,
          });
        } catch (err) {
          console.error('Failed to delete project:', err);
        }
      },

      switchProject: (id) => set({ activeProjectId: id }),
      setGroupBy: (groupBy) => set({ groupBy }),
      setSortBy: (sortBy) => set({ sortBy }),
    }),
    {
      name: 'siku.chatSidebar',
      partialize: (s) => ({
        activeProjectId: s.activeProjectId,
        groupBy: s.groupBy,
        sortBy: s.sortBy,
      }),
    }
  )
);
