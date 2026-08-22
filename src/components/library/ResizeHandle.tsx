interface ResizeHandleProps {
  onResize: (delta: number) => void;
  onResizeStart?: () => void;
  onResizeEnd?: () => void;
  className?: string;
  /**
   * 'vertical' splits left/right (col-resize, delta is clientX movement);
   * 'horizontal' splits top/bottom (row-resize, delta is clientY movement).
   */
  orientation?: 'vertical' | 'horizontal';
}

export function ResizeHandle({
  onResize,
  onResizeStart,
  onResizeEnd,
  className = '',
  orientation = 'vertical',
}: ResizeHandleProps) {
  const horizontal = orientation === 'horizontal';

  const handleMouseDown = (e: React.MouseEvent) => {
    e.preventDefault();
    const startX = e.clientX;
    const startY = e.clientY;
    onResizeStart?.();

    const handleMouseMove = (moveEvent: MouseEvent) => {
      const delta = horizontal ? moveEvent.clientY - startY : moveEvent.clientX - startX;
      onResize(delta);
    };

    const handleMouseUp = () => {
      document.removeEventListener('mousemove', handleMouseMove);
      document.removeEventListener('mouseup', handleMouseUp);
      document.body.style.cursor = '';
      onResizeEnd?.();
    };

    document.addEventListener('mousemove', handleMouseMove);
    document.addEventListener('mouseup', handleMouseUp);
    document.body.style.cursor = horizontal ? 'row-resize' : 'col-resize';
  };

  return (
    <div
      onMouseDown={handleMouseDown}
      className={`shrink-0 flex justify-center ${
        horizontal ? 'h-px w-full cursor-row-resize' : 'w-px cursor-col-resize'
      } ${className}`}
      role="separator"
      aria-orientation={horizontal ? 'horizontal' : 'vertical'}
    >
      {horizontal ? (
        <div className="h-px w-full hover:bg-primary/30 active:bg-primary/50 transition-colors" />
      ) : (
        <div className="w-px h-full hover:bg-primary/30 active:bg-primary/50 transition-colors" />
      )}
    </div>
  );
}
