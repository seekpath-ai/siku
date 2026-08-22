import { useEffect, useRef, useState } from 'react';
import { emit, listen } from '@tauri-apps/api/event';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { LogicalSize } from '@tauri-apps/api/dpi';
import { EyeOff } from 'lucide-react';
import { settingsAppGet, settingsAppSave } from '@/lib/tauri';
import './pet-window.css';

const BUBBLE_MS = 3200;

/** Root component for the always-on-top pet window. Dragging the ball starts an
 *  OS-level window move (across all screens); a plain click emits `pet:click`
 *  so the main window opens the chat panel. Messages from the main window
 *  (`pet:bubble`) appear as a speech bubble next to the ball. */
export function PetBallWindow() {
  const [bubble, setBubble] = useState<string | null>(null);
  const [menuOpen, setMenuOpen] = useState(false);
  const timerRef = useRef<number | null>(null);
  const downRef = useRef<{ x: number; y: number } | null>(null);
  const draggedRef = useRef(false);
  const menuRef = useRef<HTMLDivElement | null>(null);

  const showBubble = (text: string) => {
    setBubble(text);
    getCurrentWindow().setSize(new LogicalSize(260, 96)).catch(() => {});
    if (timerRef.current) window.clearTimeout(timerRef.current);
    timerRef.current = window.setTimeout(() => {
      setBubble(null);
      getCurrentWindow().setSize(new LogicalSize(48, 48)).catch(() => {});
    }, BUBBLE_MS);
  };

  // Listen for messages from the main window (panel opened/closed, task done).
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    const setup = async () => {
      unlisten = await listen<string>('pet:bubble', (event) => showBubble(event.payload));
    };
    setup();
    return () => {
      unlisten?.();
      if (timerRef.current) window.clearTimeout(timerRef.current);
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

  const handleMouseDown = (e: React.MouseEvent) => {
    if (e.button !== 0) return;
    downRef.current = { x: e.clientX, y: e.clientY };
    draggedRef.current = false;
  };

  const handleClick = () => {
    if (draggedRef.current) return;
    emit('pet:click').catch(() => {});
  };

  const handleContextMenu = (e: React.MouseEvent) => {
    e.preventDefault();
    setMenuOpen(true);
    getCurrentWindow().setSize(new LogicalSize(160, 96)).catch(() => {});
  };

  const handleHide = async () => {
    setMenuOpen(false);
    getCurrentWindow().setSize(new LogicalSize(48, 48)).catch(() => {});
    try {
      const current = await settingsAppGet();
      await settingsAppSave({ ...current, show_pet: false });
    } catch (err) {
      console.error('Failed to update pet setting:', err);
    }
  };

  // Close the context menu when clicking outside and restore the compact size.
  useEffect(() => {
    if (!menuOpen) return;
    const onClick = (e: MouseEvent) => {
      if (menuRef.current && !menuRef.current.contains(e.target as Node)) {
        setMenuOpen(false);
        getCurrentWindow().setSize(new LogicalSize(48, 48)).catch(() => {});
      }
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        setMenuOpen(false);
        getCurrentWindow().setSize(new LogicalSize(48, 48)).catch(() => {});
      }
    };
    window.addEventListener('mousedown', onClick);
    window.addEventListener('keydown', onKey);
    return () => {
      window.removeEventListener('mousedown', onClick);
      window.removeEventListener('keydown', onKey);
    };
  }, [menuOpen]);

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
      {bubble && (
        <div className="pet-bubble">
          <span>{bubble}</span>
          <div className="pet-bubble-arrow" />
        </div>
      )}
      {menuOpen && (
        <div
          ref={menuRef}
          onMouseDown={(e) => e.stopPropagation()}
          className="absolute top-12 left-1/2 -translate-x-1/2 z-50 min-w-[140px] py-1 bg-surface border border-surface-hover rounded-lg shadow-xl"
        >
          <button
            onClick={(e) => {
              e.stopPropagation();
              handleHide();
            }}
            className="w-full flex items-center gap-2 px-3 py-2 text-xs text-text-primary hover:bg-surface-hover transition-colors"
          >
            <EyeOff size={13} className="text-text-secondary" />
            隐藏宠物球
          </button>
        </div>
      )}
    </div>
  );
}
