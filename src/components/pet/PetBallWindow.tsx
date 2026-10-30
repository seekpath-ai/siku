import { useCallback, useEffect, useRef, useState } from 'react';
import { emit, listen } from '@tauri-apps/api/event';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { PetSprite } from './PetSprite';
import type { PetSkinDetail } from '@/lib/tauri';
import './pet-window.css';

/** Root component for the always-on-top pet window. Dragging the ball starts an
 *  OS-level window move (across all screens); a plain click emits `pet:click`
 *  so the main window opens the chat panel. Anything that needs more than the
 *  ball's square — speech bubbles, the right-click menu — lives in its own
 *  ephemeral window: this window only resizes via the size setting, because a
 *  transparent window swallows mouse events across its whole rectangle, and an
 *  enlarged rectangle that fails to shrink back becomes an invisible click trap. */
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

  // Agent state mirror (emitted by the backend for pet-domain sessions):
  // running = breathing pulse, waiting = amber + fast pulse, done/failed are
  // brief flashes that decay back to idle on a timer.
  const [agentState, setAgentState] = useState<'idle' | 'running' | 'waiting' | 'done' | 'failed'>('idle');
  const decayTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  // Custom .codex-pet skin (null = the built-in CSS ball) + click interaction
  // nonce for the sprite's wave/bounce track.
  const [skin, setSkin] = useState<PetSkinDetail | null>(null);
  const [interactNonce, setInteractNonce] = useState(0);
  const handleSpriteError = useCallback(() => setSkin(null), []);
  // Ball edge length in px (device setting `pet.size`, default 48).
  const [ballSize, setBallSize] = useState(48);
  // Drag direction while the ball moves; drives the sprite's walking tracks.
  const [moveDir, setMoveDir] = useState<'left' | 'right' | null>(null);

  useEffect(() => {
    const win = getCurrentWindow();
    let lastX: number | null = null;
    let settleTimer: ReturnType<typeof setTimeout> | null = null;
    const unlisten = win.onMoved(({ payload: pos }) => {
      if (lastX !== null) {
        const dx = pos.x - lastX;
        if (Math.abs(dx) > 2) setMoveDir(dx > 0 ? 'right' : 'left');
      }
      lastX = pos.x;
      // Movement stopped (drag released): back to the state track.
      if (settleTimer) clearTimeout(settleTimer);
      settleTimer = setTimeout(() => {
        setMoveDir(null);
        lastX = null;
      }, 250);
    });
    return () => {
      unlisten.then((u) => u());
      if (settleTimer) clearTimeout(settleTimer);
    };
  }, []);

  useEffect(() => {
    const load = async () => {
      try {
        const { petGetSize } = await import('@/lib/tauri');
        setBallSize(await petGetSize());
      } catch {
        /* keep default */
      }
    };
    load();
    let unlisten: (() => void) | undefined;
    listen<number>('pet:size_changed', (e) => setBallSize(e.payload)).then((u) => {
      unlisten = u;
    });
    return () => unlisten?.();
  }, []);

  useEffect(() => {
    const load = async () => {
      try {
        const { petGetSkinSelection, petGetSkin } = await import('@/lib/tauri');
        const id = await petGetSkinSelection();
        setSkin(id ? await petGetSkin(id) : null);
      } catch {
        setSkin(null);
      }
    };
    load();
    let unlisten: (() => void) | undefined;
    listen('pet:skin_changed', () => load()).then((u) => {
      unlisten = u;
    });
    return () => unlisten?.();
  }, []);
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    listen<string>('pet:state', (event) => {
      const s = event.payload;
      if (s !== 'idle' && s !== 'running' && s !== 'waiting' && s !== 'done' && s !== 'failed') return;
      if (decayTimerRef.current) {
        clearTimeout(decayTimerRef.current);
        decayTimerRef.current = null;
      }
      setAgentState(s);
      if (s === 'done' || s === 'failed') {
        decayTimerRef.current = setTimeout(() => setAgentState('idle'), s === 'done' ? 3000 : 5000);
      }
    }).then((u) => {
      unlisten = u;
    });
    return () => {
      unlisten?.();
      if (decayTimerRef.current) clearTimeout(decayTimerRef.current);
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

  // Hover detail card: an ephemeral window (own label 'pet-detail') shown on
  // mouse-enter with the latest pet-session status snapshot, closed on leave
  // or drag — the ball window itself never resizes.
  const closeDetail = async () => {
    const { WebviewWindow } = await import('@tauri-apps/api/webviewWindow');
    WebviewWindow.getByLabel('pet-detail')
      .then((w) => w?.close())
      .catch(() => {});
  };

  const showDetail = async () => {
    try {
      const { WebviewWindow } = await import('@tauri-apps/api/webviewWindow');
      const existing = await WebviewWindow.getByLabel('pet-detail');
      if (existing) await existing.close().catch(() => {});
      // Created hidden; PetDetailWindow positions itself under the ball,
      // fetches the status snapshot itself, goes click-through, then shows.
      new WebviewWindow('pet-detail', {
        url: 'index.html',
        title: '',
        width: 280,
        height: 220,
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
      console.error('show pet detail:', err);
    }
  };

  const handleMouseDown = (e: React.MouseEvent) => {
    if (e.button !== 0) return;
    // Dragging/clicking the ball dismisses an open menu deterministically —
    // the ball window is unfocusable, so the menu's blur-close cannot be
    // relied on for this case.
    closeMenu();
    closeDetail();
    downRef.current = { x: e.clientX, y: e.clientY };
    draggedRef.current = false;
  };

  const handleClick = () => {
    if (draggedRef.current) return;
    if (skin) setInteractNonce((n) => n + 1);
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
      onMouseEnter={showDetail}
      onMouseLeave={() => closeDetail()}
    >
      {skin ? (
        <div
          className="pet-ball-fixed cursor-pointer"
          style={{ width: ballSize - 8, height: ballSize - 8 }}
        >
          <PetSprite
            manifest={skin.manifest}
            sheetDataUrl={skin.sheetDataUrl}
            state={agentState}
            interactNonce={interactNonce}
            moveDir={moveDir}
            boxSize={ballSize - 8}
            onError={handleSpriteError}
          />
        </div>
      ) : (
        // The built-in ball is drawn at 40px; scale the wrapper so the eye /
        // mouth layout and the state animations (transform-based, on the
        // inner div) never fight the resize.
        <div
          className="pet-ball-fixed"
          style={{ transform: `scale(${(ballSize - 8) / 40})`, transformOrigin: 'top left' }}
        >
          <div className={`pet-ball pet-state-${agentState}${moveDir ? (moveDir === 'left' ? ' pet-drag-left' : ' pet-drag-right') : ''} w-10 h-10 rounded-full bg-gradient-to-br from-primary to-amber-700 flex items-center justify-center shadow-lg cursor-pointer`}>
            <div className="pet-face relative w-6 h-6">
              <div className="pet-eye absolute top-1 left-0 w-2 h-2 rounded-full bg-background" />
              <div className="pet-eye absolute top-1 right-0 w-2 h-2 rounded-full bg-background" />
              <div className="pet-mouth absolute bottom-0.5 left-1/2 -translate-x-1/2 w-3 h-1.5 rounded-b-full bg-background/80" />
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
