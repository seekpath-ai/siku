import { useCallback, useEffect, useRef, useState } from 'react';
import { listen, emit } from '@tauri-apps/api/event';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { useNavigate } from '@tanstack/react-router';
import { X, Bot, Minus, Square, Copy, ExternalLink } from 'lucide-react';
import { usePetStore } from '@/stores/petStore';
import { usePetContextStore } from '@/stores/petContextStore';
import { useEvidenceStore } from '@/stores/evidenceStore';
import { PetConversation, DOMAIN_ID_NAMES, DOMAIN_NAMES } from './PetConversation';

/** Floating pet panel in the MAIN window: a draggable shell around
 *  PetConversation. The same conversation also runs inside the standalone
 *  PetChatWindow — session state lives in the backend and `agent:event` is
 *  broadcast to every webview, so both stay in sync. */
export function Pet() {
  const { context } = usePetContextStore();
  const store = usePetStore();
  const [panelPos, setPanelPos] = useState<{ x: number; y: number } | null>(null);
  const [maximized, setMaximized] = useState(false);
  const panelDragRef = useRef<{ dx: number; dy: number } | null>(null);

  // Show a speech bubble next to the floating pet ball (separate window).
  const notify = useCallback(async (body: string) => {
    emit('pet:bubble', body).catch(() => {});
  }, []);

  // Drag the panel by its header (DOM-level, no OS window involved).
  useEffect(() => {
    const onMove = (e: MouseEvent) => {
      const d = panelDragRef.current;
      if (!d) return;
      setPanelPos({ x: e.clientX - d.dx, y: e.clientY - d.dy });
    };
    const onUp = () => {
      panelDragRef.current = null;
    };
    window.addEventListener('mousemove', onMove);
    window.addEventListener('mouseup', onUp);
    return () => {
      window.removeEventListener('mousemove', onMove);
      window.removeEventListener('mouseup', onUp);
    };
  }, []);

  const handleHeaderMouseDown = (e: React.MouseEvent) => {
    if (e.button !== 0 || maximized) return; // no dragging while maximized
    const rect = (e.currentTarget as HTMLElement).getBoundingClientRect();
    panelDragRef.current = { dx: e.clientX - rect.left, dy: e.clientY - rect.top };
  };

  // Minimize: collapse the panel but keep the session (reopen via the ball).
  const handleMinimize = () => {
    store.setOpen(false);
  };

  const handleClose = () => {
    store.setOpen(false);
    store.reset();
    notify('对话面板已关闭');
  };

  // Pop the conversation out into its own always-draggable OS window (like
  // "open note in new window"). The main panel stays open; both mirror the
  // same backend session.
  const handlePopOut = async () => {
    const st = usePetStore.getState();
    if (!st.session) return;
    const ctx = usePetContextStore.getState().context;
    try {
      const { WebviewWindow } = await import('@tauri-apps/api/webviewWindow');
      const params = new URLSearchParams({ petSession: st.session.id });
      if (ctx) {
        params.set('page', ctx.page);
        params.set('objectId', ctx.objectId);
        params.set('title', ctx.title);
      }
      const label = `pet-chat-${Date.now()}`;
      new WebviewWindow(label, {
        url: `index.html?${params.toString()}`,
        title: '智能助手 - 思库',
        width: 440,
        height: 680,
        minWidth: 360,
        minHeight: 480,
        center: true,
        decorations: false,
        transparent: true,
        shadow: false,
      });
    } catch (err) {
      console.error('pop out pet chat:', err);
    }
  };

  // Evidence citation clicks from a detached pet chat window are forwarded
  // here: record the highlight request and open the reader on that paper.
  const navigate = useNavigate();
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    listen<{ paperId: string; page?: number; exact: string }>(
      'pet:evidence-highlight',
      (event) => {
        if (getCurrentWindow().label !== 'main') return;
        getCurrentWindow().setFocus().catch(() => {});
        useEvidenceStore.getState().requestHighlight(event.payload);
        navigate({ to: '/reader/$paperId', params: { paperId: event.payload.paperId } });
      },
    ).then((u) => { unlisten = u; });
    return () => unlisten?.();
  }, [navigate]);

  // Open the panel when the floating pet ball (separate window) is clicked.
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let cancelled = false;
    const setup = async () => {
      unlisten = await listen('pet:click', () => {
        if (cancelled) return;
        // Only the main window shows the pet panel — pet:click is broadcast
        // to every webview (e.g. "open in new window" note windows).
        if (getCurrentWindow().label !== 'main') return;
        // Bring the main window to the front so the panel is visible.
        getCurrentWindow().setFocus().catch(() => {});
        const st = usePetStore.getState();
        const ctx = usePetContextStore.getState().context;
        if (ctx && !st.session) {
          st.start(ctx);
        } else {
          st.setOpen(true);
        }
        notify('已为您打开对话面板');
      });
    };
    setup();
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [notify]);

  if (!store.open) return null;

  return (
    <div
      className={`fixed z-[4000] flex flex-col bg-surface border border-surface-hover rounded-2xl shadow-2xl overflow-hidden ${
        maximized ? 'w-[70vw] h-[70vh]' : 'w-[380px] max-w-[92vw] max-h-[min(70vh,560px)]'
      }`}
      style={maximized ? { left: '15vw', top: '15vh' } : panelPos ? { left: panelPos.x, top: panelPos.y } : { right: 16, bottom: 16 }}
    >
      {/* Header (drag handle) */}
      <div
        onMouseDown={handleHeaderMouseDown}
        className="flex items-center gap-2 px-3 py-2.5 border-b border-surface-hover shrink-0 cursor-grab active:cursor-grabbing"
      >
        <div className="w-6 h-6 rounded-full bg-gradient-to-br from-primary to-amber-700 flex items-center justify-center">
          <Bot size={13} className="text-background" />
        </div>
        <span className="text-[13px] font-semibold text-text-primary">
          {store.session
            ? DOMAIN_ID_NAMES[store.session.domain ?? ''] || '智能助手'
            : context
              ? DOMAIN_NAMES[context.page] || '智能助手'
              : '智能助手'}
        </span>
        {/* Window-style controls (match the main TitleBar) */}
        <div className="flex items-center gap-0.5 shrink-0 ml-auto -mr-1">
          <button
            onClick={handlePopOut}
            disabled={!store.session}
            title="在新窗口中打开"
            className="w-7 h-7 flex items-center justify-center rounded text-text-secondary/70 hover:text-text-primary hover:bg-surface-hover transition-colors disabled:opacity-40 disabled:cursor-not-allowed"
          >
            <ExternalLink size={12} strokeWidth={1.5} />
          </button>
          <button
            onClick={handleMinimize}
            title="最小化"
            className="w-7 h-7 flex items-center justify-center rounded text-text-secondary/70 hover:text-text-primary hover:bg-surface-hover transition-colors"
          >
            <Minus size={13} strokeWidth={1.5} />
          </button>
          <button
            onClick={() => setMaximized((m) => !m)}
            title={maximized ? '还原' : '最大化'}
            className="w-7 h-7 flex items-center justify-center rounded text-text-secondary/70 hover:text-text-primary hover:bg-surface-hover transition-colors"
          >
            {maximized ? <Copy size={12} strokeWidth={1.5} /> : <Square size={12} strokeWidth={1.5} />}
          </button>
          <button
            onClick={handleClose}
            title="关闭"
            className="w-7 h-7 flex items-center justify-center rounded text-text-secondary/70 hover:text-white hover:bg-red-500/80 transition-colors"
          >
            <X size={14} strokeWidth={1.5} />
          </button>
        </div>
      </div>

      <PetConversation context={context} />
    </div>
  );
}
