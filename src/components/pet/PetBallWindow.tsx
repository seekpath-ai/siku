import { useEffect, useRef } from 'react';
import { emit, listen } from '@tauri-apps/api/event';
import { getCurrentWindow } from '@tauri-apps/api/window';
import './pet-window.css';

/** Root component for the always-on-top pet window. Dragging the ball starts an
 *  OS-level window move (across all screens); a plain click emits `pet:click`
 *  so the main window opens the chat panel. Anything that needs more than the
 *  ball's 48×48 — speech bubbles, the right-click menu — lives in its own
 *  ephemeral window: this window NEVER resizes, because a transparent window
 *  swallows mouse events across its whole rectangle, and an enlarged
 *  rectangle that fails to shrink back becomes an invisible click trap. */
export function PetBallWindow() {
  const downRef = useRef<{ x: number; y: number } | null>(null);
  const draggedRef = useRef(false);

  const showBubble = async (text: string) => {
    try {
      const { WebviewWindow } = await import('@tauri-apps/api/webviewWindow');
      const existing = await WebviewWindow.getByLabel('pet-bubble');
      if (existing) await existing.close().catch(() => {});
      // Created hidden; PetBubbleWindow positions itself under the ball,
      // goes click-through, then shows and self-closes after a timeout.
      new WebviewWindow('pet-bubble', {
        url: `index.html?petBubble=1&text=${encodeURIComponent(text)}`,
        title: '',
        width: 260,
        height: 96,
        decorations: false,
        transparent: true,
        shadow: false,
        alwaysOnTop: true,
        skipTaskbar: true,
        resizable: false,
        focus: false,
        visible: false,
      });
    } catch (err) {
      console.error('show pet bubble:', err);
    }
  };

  // Right-click menu lives in a separate ephemeral window (PetMenuWindow)
  // that closes on blur — an in-window menu forced the ball window to grow,
  // and its "click outside to dismiss" listener could never observe clicks
  // on the desktop, so the enlarged hit region regularly got stuck.
  const showMenu = async () => {
    try {
      const { WebviewWindow } = await import('@tauri-apps/api/webviewWindow');
      const existing = await WebviewWindow.getByLabel('pet-menu');
      if (existing) await existing.close().catch(() => {});
      // Created hidden; PetMenuWindow positions itself under the ball, then
      // shows focused and closes on blur.
      new WebviewWindow('pet-menu', {
        url: 'index.html?petMenu=1',
        title: '',
        width: 160,
        height: 48,
        decorations: false,
        transparent: true,
        shadow: false,
        alwaysOnTop: true,
        skipTaskbar: true,
        resizable: false,
        visible: false,
      });
    } catch (err) {
      console.error('show pet menu:', err);
    }
  };

  // Listen for messages from the main window (panel opened/closed, task done).
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    const setup = async () => {
      unlisten = await listen<string>('pet:bubble', (event) => {
        showBubble(event.payload);
      });
    };
    setup();
    return () => {
      unlisten?.();
    };
  }, []);

  // Start the window drag only after the pointer actually moves past a small
  // threshold, so a plain click still fires and isn't swallowed by dragging.
  useEffect(() => {
    const onMove = (e: MouseEvent) => {
      const d = downRef.current;
      if (!d || draggedRef.current) return;
      if (Math.hypot(e.clientX - d.x, e.clientY - d.y) < 5) return;
      draggedRef.current = true;
      getCurrentWindow().startDragging().catch(() => {});
    };
    const onUp = () => {
      downRef.current = null;
      // Keep the dragged flag until the click event has fired, then clear it.
      setTimeout(() => {
        draggedRef.current = false;
      }, 0);
    };
    window.addEventListener('mousemove', onMove);
    window.addEventListener('mouseup', onUp);
    return () => {
      window.removeEventListener('mousemove', onMove);
      window.removeEventListener('mouseup', onUp);
    };
  }, []);

  const closeMenu = async () => {
    const { WebviewWindow } = await import('@tauri-apps/api/webviewWindow');
    WebviewWindow.getByLabel('pet-menu')
      .then((w) => w?.close())
      .catch(() => {});
  };

  const handleMouseDown = (e: React.MouseEvent) => {
    if (e.button !== 0) return;
    // Dragging/clicking the ball dismisses an open menu deterministically —
    // the ball window is unfocusable, so the menu's blur-close cannot be
    // relied on for this case.
    closeMenu();
    downRef.current = { x: e.clientX, y: e.clientY };
    draggedRef.current = false;
  };

  const handleClick = () => {
    if (draggedRef.current) return;
    emit('pet:click').catch(() => {});
  };

  const handleContextMenu = (e: React.MouseEvent) => {
    e.preventDefault();
    showMenu();
  };

  return (
    <div
      className="pet-window"
      onMouseDown={handleMouseDown}
      onClick={handleClick}
      onContextMenu={handleContextMenu}
      title="点击打开智能体，右键可隐藏"
    >
      <div className="pet-ball pet-ball-fixed w-10 h-10 rounded-full bg-gradient-to-br from-primary to-amber-700 flex items-center justify-center shadow-lg cursor-pointer">
        <div className="relative w-6 h-6">
          <div className="pet-eye absolute top-1 left-0 w-2 h-2 rounded-full bg-background" />
          <div className="pet-eye absolute top-1 right-0 w-2 h-2 rounded-full bg-background" />
          <div className="absolute bottom-0.5 left-1/2 -translate-x-1/2 w-3 h-1.5 rounded-b-full bg-background/80" />
        </div>
      </div>
    </div>
  );
}
