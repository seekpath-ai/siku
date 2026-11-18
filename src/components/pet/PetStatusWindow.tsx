import { useCallback, useEffect, useState } from 'react';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { emit, listen } from '@/lib/events';
import { agentApproveTool, petCurrentStatus, settingsAppGet } from '@/lib/tauri';
import type { PetActivity, PetStatusInfo } from '@/lib/tauri';
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
 *  clicking the card opens the pet panel; a waiting chat activity carries
 *  inline controls so the interaction doesn't require opening the panel. */
export function PetStatusWindow() {
  const [status, setStatus] = useState<PetStatusInfo | null>(null);
  const [placement, setPlacement] = useState<BallPlacement | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(async () => {
    try {
      setStatus(await petCurrentStatus());
    } catch {
      /* keep last snapshot */
    }
  }, []);

  useEffect(() => {
    let cancelled = false;
    const refresh = async () => {
      const st = await petCurrentStatus().catch(() => null);
      if (!cancelled) setStatus(st);
    };
    refresh();
    let unlisten: (() => void) | undefined;
    listen<string>('pet:state', () => refresh()).then((u) => {
      unlisten = u;
    });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);

  // The ball is hidden: this overlay must not outlive it. The backend also
  // closes overlays on the visibility transition; this is the frontend guard
  // for a window that was already open before that event landed.
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let cancelled = false;
    const check = async () => {
      const settings = await settingsAppGet().catch(() => null);
      if (!cancelled && settings && !settings.show_pet) {
        getCurrentWindow().close().catch(() => {});
      }
    };
    check();
    listen('app:settings_changed', () => void check()).then((u) => {
      unlisten = u;
    });
    return () => {
      cancelled = true;
      unlisten?.();
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
    let unlisten: (() => void) | undefined;
    listen('pet:ball-moved', () => place()).then((u) => {
      unlisten = u;
    });
    return () => unlisten?.();
  }, []);

  const openPanel = useCallback(() => {
    emit('pet:click').catch(() => {});
  }, []);

  const decide = useCallback(
    async (activity: PetActivity, decision: 'approve' | 'decline') => {
      const sessionId = activity.sessionId;
      if (!sessionId || busy) return;
      setBusy(sessionId);
      setError(null);
      try {
        await agentApproveTool(sessionId, activity.toolCallId ?? '', decision);
        // The engine may not report a new state until the approved tool
        // finishes. Flip this activity locally so the card doesn't keep
        // offering 批准/拒绝 while the tool is already running.
        setStatus((prev) => {
          if (!prev) return prev;
          const activities = prev.activities.map((a) =>
            a.sessionId === sessionId
              ? { ...a, state: 'running', waitingKind: undefined, toolCallId: undefined }
              : a,
          );
          return {
            ...prev,
            state: activities.some((a) => a.state === 'waiting') ? 'waiting' : 'running',
            activities,
          };
        });
      } catch (err) {
        setError(
          `审批提交失败：${err instanceof Error ? err.message : String(err)}。` +
            '请在对话面板中处理。',
        );
        await load();
      } finally {
        setBusy(null);
      }
    },
    [busy, load],
  );

  /** AskUserQuestion has no tool-approval channel: open the conversation that
   *  owns the question instead of submitting a bogus approval. */
  const answerQuestion = useCallback((activity: PetActivity) => {
    if (!activity.sessionId) return;
    emit('pet:open-session', {
      sessionId: activity.sessionId,
      target: activity.openTarget ?? 'chat',
    }).catch(() => {});
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
        {activities.map((a, i) => {
          const isApproval = a.waitingKind === 'approval';
          const isQuestion = a.waitingKind === 'question';
          const isWaiting = a.state === 'waiting';
          return (
            <div key={`${a.sessionId ?? a.label}-${i}`} className="pet-detail-task">
              <span className={`pet-detail-dot pet-detail-dot-${a.state}`} />
              <div className="pet-detail-task-body">
                <div className="pet-detail-title">{a.label}</div>
                {a.detail && <div className="pet-detail-row pet-detail-prompt">{a.detail}</div>}
                {isWaiting && a.sessionId && (
                  <div className="flex gap-1.5 mt-1">
                    {isApproval && (
                      <>
                        <button
                          onClick={(e) => {
                            e.stopPropagation();
                            void decide(a, 'approve');
                          }}
                          disabled={busy === a.sessionId}
                          className="px-2 py-0.5 rounded bg-primary/15 text-primary text-[11px] hover:bg-primary/25 transition-colors disabled:opacity-50"
                        >
                          批准
                        </button>
                        <button
                          onClick={(e) => {
                            e.stopPropagation();
                            void decide(a, 'decline');
                          }}
                          disabled={busy === a.sessionId}
                          className="px-2 py-0.5 rounded bg-red-500/10 text-red-400 text-[11px] hover:bg-red-500/20 transition-colors disabled:opacity-50"
                        >
                          拒绝
                        </button>
                      </>
                    )}
                    {isQuestion && (
                      <button
                        onClick={(e) => {
                          e.stopPropagation();
                          answerQuestion(a);
                        }}
                        className="px-2 py-0.5 rounded bg-primary/15 text-primary text-[11px] hover:bg-primary/25 transition-colors"
                      >
                        回答
                      </button>
                    )}
                    {!isApproval && !isQuestion && (
                      <button
                        onClick={(e) => {
                          e.stopPropagation();
                          answerQuestion(a);
                        }}
                        className="px-2 py-0.5 rounded bg-surface-hover text-text-secondary text-[11px] hover:text-text-primary transition-colors"
                      >
                        打开对话
                      </button>
                    )}
                  </div>
                )}
              </div>
            </div>
          );
        })}
        {error && (
          <div className="mt-1.5 text-[10px] leading-snug text-red-400">{error}</div>
        )}
      </div>
    </div>
  );
}
