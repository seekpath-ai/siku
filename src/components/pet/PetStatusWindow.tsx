import { useCallback, useEffect, useState } from 'react';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { emit, listen } from '@/lib/events';
import { agentApproveTool, petCurrentStatus } from '@/lib/tauri';
import type { PetStatusInfo } from '@/lib/tauri';
import { placeNearBall } from './overlayPlacement';
import type { BallPlacement } from './overlayPlacement';
import './pet-window.css';

const STATE_LABEL: Record<string, string> = {
  running: '思考 / 工作中',
  waiting: '等待确认',
  done: '已完成',
  failed: '出错了',
  idle: '空闲',
};

/** Persistent status card anchored to the pet ball while the agent is busy —
 *  the Codex-style always-visible counterpart of the hover detail card.
 *  Created by PetBallWindow when the aggregate state turns running/waiting,
 *  closed by it on idle. Unlike the hover card this one is INTERACTIVE:
 *  clicking the card opens the pet panel, and a waiting chat activity carries
 *  inline 批准/拒绝 buttons so approvals don't require opening the panel. */
export function PetStatusWindow() {
  const [status, setStatus] = useState<PetStatusInfo | null>(null);
  const [placement, setPlacement] = useState<BallPlacement | null>(null);

  useEffect(() => {
    let cancelled = false;
    const load = async () => {
      try {
        const st = await petCurrentStatus();
        if (!cancelled) setStatus(st);
      } catch {
        /* keep last snapshot */
      }
    };
    load();
    const unlisten = listen<string>('pet:state', () => load());
    return () => {
      cancelled = true;
      unlisten.then((u) => u());
    };
  }, []);

  // Anchor ABOVE the ball (Codex-style status card over the pet's head;
  // placeNearBall flips below when the ball hugs the screen top), then show.
  // NOT click-through (the approval buttons need pointer events), and never
  // focused — the card appears while the user may be typing elsewhere.
  // Re-anchors whenever the ball moves (dragging it across monitors included).
  useEffect(() => {
    const self = getCurrentWindow();
    const place = () =>
      placeNearBall('above')
        .then(setPlacement)
        .catch(() => null);
    place().then(() => self.show().catch(() => {}));
    const unlisten = listen('pet:ball-moved', () => place());
    return () => {
      unlisten.then((u) => u());
    };
  }, []);

  const openPanel = useCallback(() => {
    emit('pet:click').catch(() => {});
  }, []);

  const decide = useCallback(async (sessionId: string, decision: 'approve' | 'decline') => {
    try {
      // Empty toolCallId: the backend resolves it from the session's pending
      // approval context.
      await agentApproveTool(sessionId, '', decision);
    } catch (err) {
      console.error('status card approval failed:', err);
    }
  }, []);

  const state = status?.state ?? 'idle';
  const activities = status?.activities ?? [];

  return (
    <div className={`pet-status-window${placement?.side === 'above' ? ' pet-status-above' : ''}`}>
      <div
        className={`pet-detail pet-detail-${state} cursor-pointer select-none`}
        onClick={openPanel}
        title="点击打开对话面板"
      >
        <div className="pet-detail-state">{STATE_LABEL[state] ?? state}</div>
        {activities.map((a, i) => (
          <div key={i} className="pet-detail-task">
            <span className={`pet-detail-dot pet-detail-dot-${a.state}`} />
            <div className="pet-detail-task-body">
              <div className="pet-detail-title">{a.label}</div>
              {a.detail && <div className="pet-detail-row pet-detail-prompt">{a.detail}</div>}
              {a.state === 'waiting' && a.sessionId && (
                <div className="flex gap-1.5 mt-1">
                  <button
                    onClick={(e) => {
                      e.stopPropagation();
                      void decide(a.sessionId!, 'approve');
                    }}
                    className="px-2 py-0.5 rounded bg-primary/15 text-primary text-[11px] hover:bg-primary/25 transition-colors"
                  >
                    批准
                  </button>
                  <button
                    onClick={(e) => {
                      e.stopPropagation();
                      void decide(a.sessionId!, 'decline');
                    }}
                    className="px-2 py-0.5 rounded bg-red-500/10 text-red-400 text-[11px] hover:bg-red-500/20 transition-colors"
                  >
                    拒绝
                  </button>
                </div>
              )}
            </div>
          </div>
        ))}
      </div>
    </div>
  );
}
