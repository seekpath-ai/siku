import { useEffect, useState } from 'react';
import { X, Bot } from 'lucide-react';
import { listen } from '@/lib/events';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { usePetStore } from '@/stores/petStore';
import type { PetContext } from '@/stores/petContextStore';
import { PetConversation, DOMAIN_ID_NAMES } from './PetConversation';
import { Dialog } from '@/components/ui/Dialog';

/** Standalone pet chat window ("pop the panel out of the main window").
 *  Reads the session id and display context from its URL
 *  (?petSession=<id>&page=<p>&objectId=<oid>&title=<t>), attaches the store
 *  to the existing backend session, and renders the shared conversation.
 *  Actions that need the main window's LIVE text selection are hidden. */
export function PetChatWindow() {
  const store = usePetStore();
  const [context, setContext] = useState<PetContext | null>(null);
  const [paramError, setParamError] = useState<string | null>(null);
  const [reviewStatus, setReviewStatus] = useState<string | null>(null);
  const [reviewError, setReviewError] = useState<string | null>(null);

  useEffect(() => {
    const params = new URLSearchParams(window.location.search);
    const sessionId = params.get('petSession');
    const page = params.get('page');
    if (page) {
      setContext({
        page,
        objectId: params.get('objectId') ?? '',
        title: params.get('title') ?? '',
      });
    }
    if (!sessionId) {
      setParamError('缺少会话参数');
      return;
    }
    usePetStore.getState().attach(sessionId);
  }, []);

  useEffect(() => {
    const params = new URLSearchParams(window.location.search);
    const sessionId = params.get('petSession');
    if (!sessionId) return;
    let unProgress: (() => void) | undefined;
    let unError: (() => void) | undefined;
    listen<{ sessionId: string; message: string }>('skills:review-progress', (event) => {
      if (event.payload.sessionId === sessionId) setReviewStatus(event.payload.message);
    }).then((u) => { unProgress = u; });
    listen<{ sessionId: string; message: string }>('skills:review-error', (event) => {
      if (event.payload.sessionId === sessionId) {
        setReviewStatus(null);
        setReviewError(event.payload.message);
      }
    }).then((u) => { unError = u; });
    return () => {
      unProgress?.();
      unError?.();
    };
  }, []);

  // The session was cleared from another window: nothing left to show.
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    listen<string>('pet:session-cleared', (event) => {
      const st = usePetStore.getState();
      if (st.session?.id !== event.payload) return;
      st.reset();
      getCurrentWindow().close().catch(() => {});
    }).then((u) => { unlisten = u; });
    return () => unlisten?.();
  }, []);

  return (
    <div className="h-screen w-screen rounded-xl overflow-hidden border border-surface-hover flex flex-col bg-background">
      {/* Mini title bar (drag + close) */}
      <div
        data-tauri-drag-region="deep"
        className="titlebar-drag flex items-center gap-2 h-[36px] px-2 bg-surface border-b border-surface-hover shrink-0 select-none"
      >
        <div className="w-5 h-5 rounded-full bg-gradient-to-br from-primary to-amber-700 flex items-center justify-center shrink-0">
          <Bot size={11} className="text-background" />
        </div>
        <span className="text-xs font-medium text-text-secondary flex-1 truncate">
          {store.session
            ? DOMAIN_ID_NAMES[store.session.domain ?? ''] || '智能助手'
            : '智能助手'}
        </span>
        <button
          onClick={() => getCurrentWindow().close().catch(() => {})}
          className="w-8 h-8 flex items-center justify-center text-text-secondary hover:text-white hover:bg-red-500/80 transition-colors"
          title="关闭"
        >
          <X size={16} strokeWidth={1.5} />
        </button>
      </div>

      {/* Slim windows have no AppShell — mount the shared dialog host so
          PetConversation's confirm/alert dialogs render here too. */}
      <Dialog />

      {paramError ? (
        <div className="flex-1 flex items-center justify-center text-xs text-red-400">
          {paramError}
        </div>
      ) : (
        <>
          {reviewStatus && (
            <div className="mx-3 mt-2 rounded-lg border border-primary/20 bg-primary/5 px-3 py-2 text-[11px] text-primary">
              {reviewStatus}
            </div>
          )}
          {reviewError && (
            <div className="mx-3 mt-2 rounded-lg border border-red-500/20 bg-red-500/5 px-3 py-2 text-[11px] text-red-400">
              审查失败：{reviewError}
            </div>
          )}
          <PetConversation context={context} liveSelection={false} />
        </>
      )}
    </div>
  );
}
