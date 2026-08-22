import { useRef, useState } from 'react';
import { useShellStore } from '@/stores/shellStore';
import { ResizeHandle } from '@/components/library/ResizeHandle';

interface ListPanelProps {
  children: React.ReactNode;
  width?: number;
  collapseThreshold?: number;
  minWidth?: number;
}

export function ListPanel({
  children,
  width: defaultWidth = 256,
  collapseThreshold = 256,
  minWidth = 180,
}: ListPanelProps) {
  const { sidePanelCollapsed, setSidePanelCollapsed } = useShellStore();
  const [width, setWidth] = useState(defaultWidth);
  const [isResizing, setIsResizing] = useState(false);
  const dragStartCollapsed = useRef(false);
  const dragStartWidth = useRef(width);

  const displayedWidth = sidePanelCollapsed ? 0 : width;

  const handleResizeStart = () => {
    dragStartCollapsed.current = sidePanelCollapsed;
    dragStartWidth.current = sidePanelCollapsed ? defaultWidth : width;
    setIsResizing(true);
  };

  const handleResizeEnd = () => {
    setIsResizing(false);
  };

  const handleResize = (delta: number) => {
    if (dragStartCollapsed.current) {
      // Dragging right from collapsed state: expand once threshold is crossed.
      const newWidth = dragStartWidth.current + delta;
      if (newWidth >= collapseThreshold) {
        setSidePanelCollapsed(false);
        setWidth(newWidth);
      }
    } else {
      const newWidth = dragStartWidth.current + delta;
      if (newWidth < collapseThreshold) {
        setSidePanelCollapsed(true);
      } else {
        setWidth(Math.max(minWidth, newWidth));
      }
    }
  };

  return (
    <>
      <div
        className={`shrink-0 overflow-hidden h-full ${
          isResizing ? '' : 'transition-all duration-200 ease-out'
        }`}
        style={{ width: displayedWidth }}
      >
        <div style={{ width }} className="h-full">
          {children}
        </div>
      </div>
      <ResizeHandle
        onResizeStart={handleResizeStart}
        onResize={handleResize}
        onResizeEnd={handleResizeEnd}
        className="bg-surface-hover/50"
      />
    </>
  );
}
