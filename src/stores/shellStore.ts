import { create } from 'zustand';
import { persist } from 'zustand/middleware';

interface ShellState {
  sidePanelCollapsed: boolean;
  toggleSidePanel: () => void;
  setSidePanelCollapsed: (collapsed: boolean) => void;
  isMaximized: boolean;
  setIsMaximized: (maximized: boolean) => void;
}

const STORAGE_KEY = 'siku.shell';

export const useShellStore = create<ShellState>()(
  persist(
    (set, get) => ({
      sidePanelCollapsed: false,
      isMaximized: false,
      toggleSidePanel: () => set({ sidePanelCollapsed: !get().sidePanelCollapsed }),
      setSidePanelCollapsed: (collapsed) => set({ sidePanelCollapsed: collapsed }),
      setIsMaximized: (maximized) => set({ isMaximized: maximized }),
    }),
    {
      name: STORAGE_KEY,
      partialize: (state) => ({
        sidePanelCollapsed: state.sidePanelCollapsed,
      }),
    }
  )
);
