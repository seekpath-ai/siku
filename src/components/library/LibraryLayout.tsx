import { useEffect, useRef, useState } from 'react';
import { listen } from '@tauri-apps/api/event';
import { useQueryClient } from '@tanstack/react-query';
import { useLibraryStore } from '@/stores/libraryStore';
import { useShellStore } from '@/stores/shellStore';
import { CollectionTree } from './CollectionTree';
import { PaperList } from './PaperList';
import { PaperDetailPanel } from './PaperDetailPanel';
import { ResizeHandle } from './ResizeHandle';

const RIGHT_PANEL_COLLAPSE_THRESHOLD = 140;
const RIGHT_PANEL_COLLAPSED_WIDTH = 44;
const LEFT_PANEL_COLLAPSE_THRESHOLD = 256;
const LEFT_PANEL_DEFAULT_WIDTH = 256;

export function LibraryLayout() {
  const queryClient = useQueryClient();
  const leftWidth = useLibraryStore((s) => s.leftPanelWidth);
  const rightWidth = useLibraryStore((s) => s.rightPanelWidth);
  const rightPanelCollapsed = useLibraryStore((s) => s.rightPanelCollapsed);
  const setLeftWidth = useLibraryStore((s) => s.setLeftPanelWidth);
  const setRightWidth = useLibraryStore((s) => s.setRightPanelWidth);
  const setRightPanelCollapsed = useLibraryStore((s) => s.setRightPanelCollapsed);
  const sidePanelCollapsed = useShellStore((s) => s.sidePanelCollapsed);
  const setSidePanelCollapsed = useShellStore((s) => s.setSidePanelCollapsed);

  const [isResizingLeft, setIsResizingLeft] = useState(false);
  const [isResizingRight, setIsResizingRight] = useState(false);

  const rightDragStartCollapsed = useRef(false);
  const rightDragStartWidth = useRef(rightWidth);

  // Reload library data when sync applies remote changes (P2P or the offline
  // mailbox path). The notes page already does this; without it, papers /
  // attachments / collections / tags arriving from another device never show
  // up in the library view because the queries are cached (staleTime 30s) and
  // only refetch on remount or explicit refresh. Debounced because changesets
  // and mailbox batches can arrive in bursts.
  useEffect(() => {
    let timer: ReturnType<typeof setTimeout> | null = null;
    const unlisten = listen('sync:remote_applied', () => {
      if (timer) clearTimeout(timer);
      timer = setTimeout(() => {
        queryClient.invalidateQueries({ queryKey: ['papers'] });
        queryClient.invalidateQueries({ queryKey: ['paper'] });
        queryClient.invalidateQueries({ queryKey: ['paper-notes'] });
        queryClient.invalidateQueries({ queryKey: ['paper-tags'] });
        queryClient.invalidateQueries({ queryKey: ['paper-related'] });
        queryClient.invalidateQueries({ queryKey: ['paper-attachments'] });
        queryClient.invalidateQueries({ queryKey: ['collections'] });
        queryClient.invalidateQueries({ queryKey: ['tags'] });
        queryClient.invalidateQueries({ queryKey: ['saved-searches'] });
      }, 500);
    });
    return () => {
      if (timer) clearTimeout(timer);
      unlisten.then((fn) => fn());
    };
  }, [queryClient]);

  const leftDragStartCollapsed = useRef(false);
  const leftDragStartWidth = useRef(leftWidth);

  const handleLeftResizeStart = () => {
    leftDragStartCollapsed.current = sidePanelCollapsed;
    leftDragStartWidth.current = sidePanelCollapsed ? LEFT_PANEL_DEFAULT_WIDTH : leftWidth;
    setIsResizingLeft(true);
  };

  const handleLeftResizeEnd = () => {
    setIsResizingLeft(false);
  };

  const handleLeftResize = (delta: number) => {
    if (leftDragStartCollapsed.current) {
      // Dragging right from collapsed state: expand once threshold is crossed.
      const newWidth = leftDragStartWidth.current + delta;
      if (newWidth >= LEFT_PANEL_COLLAPSE_THRESHOLD) {
        setSidePanelCollapsed(false);
        setLeftWidth(newWidth);
      }
    } else {
      const newWidth = leftDragStartWidth.current + delta;
      if (newWidth < LEFT_PANEL_COLLAPSE_THRESHOLD) {
        setSidePanelCollapsed(true);
      } else {
        setLeftWidth(newWidth);
      }
    }
  };

  const displayedRightWidth = rightPanelCollapsed ? RIGHT_PANEL_COLLAPSED_WIDTH : rightWidth;

  const handleRightResize = (delta: number) => {
    if (rightDragStartCollapsed.current) {
      // Dragging from collapsed state: left drag (negative delta) expands
      const newWidth = rightDragStartWidth.current - delta;
      if (newWidth >= RIGHT_PANEL_COLLAPSE_THRESHOLD) {
        setRightPanelCollapsed(false);
        setRightWidth(newWidth);
      }
    } else {
      const newWidth = rightDragStartWidth.current - delta;
      if (newWidth < RIGHT_PANEL_COLLAPSE_THRESHOLD) {
        setRightPanelCollapsed(true);
      } else {
        setRightWidth(newWidth);
      }
    }
  };

  return (
    <div className="flex h-full bg-background overflow-hidden">
      {/* Left: collections & tags */}
      <aside
        className={`shrink-0 bg-surface/30 flex flex-col overflow-hidden ${
          isResizingLeft ? '' : 'transition-all duration-200 ease-out'
        }`}
        style={{ width: sidePanelCollapsed ? 0 : leftWidth }}
      >
        <div style={{ width: leftWidth }} className="h-full">
          <CollectionTree />
        </div>
      </aside>

      <ResizeHandle
        onResizeStart={handleLeftResizeStart}
        onResizeEnd={handleLeftResizeEnd}
        onResize={handleLeftResize}
        className="bg-surface-hover/50"
      />

      {/* Center: paper list */}
      <main className="flex-1 min-w-0 flex flex-col">
        <PaperList />
      </main>

      <ResizeHandle
        onResizeStart={() => {
          rightDragStartCollapsed.current = rightPanelCollapsed;
          rightDragStartWidth.current = rightWidth;
          setIsResizingRight(true);
        }}
        onResizeEnd={() => setIsResizingRight(false)}
        onResize={handleRightResize}
        className="bg-surface-hover/50"
      />

      {/* Right: detail panel */}
      <aside
        className={`shrink-0 bg-surface/30 flex flex-col overflow-hidden ${
          isResizingRight ? '' : 'transition-all duration-200 ease-out'
        }`}
        style={{ width: displayedRightWidth }}
      >
        <div style={{ width: rightPanelCollapsed ? RIGHT_PANEL_COLLAPSED_WIDTH : rightWidth }} className="h-full">
          <PaperDetailPanel />
        </div>
      </aside>
    </div>
  );
}
