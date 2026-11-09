import { useEffect, useState } from 'react';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { listen } from '@/lib/events';
import { petCurrentStatus } from '@/lib/tauri';
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

/** Ephemeral hover detail card shown above the pet ball (created by
 *  PetBallWindow on mouse-enter, closed on leave) — same anchor spot as the
 *  busy-time status card, so status info always lives over the pet's head.
 *  Same pattern as the speech bubble: own always-on-top transparent window so
 *  the ball window itself never has to resize; click-through so it never
 *  steals pointer events. Fetches the status itself (supporting multiple
 *  concurrent tasks) and refreshes whenever the aggregate state changes. */
export function PetDetailWindow() {
  const [status, setStatus] = useState<PetStatusInfo | null>(null);
  const [placement, setPlacement] = useState<BallPlacement | null>(null);

  useEffect(() => {
    let cancelled = false;
    const load = async () => {
      try {
        const st = await petCurrentStatus();
        if (!cancelled) setStatus(st);
      } catch {
        /* show idle */
      }
    };
    load();
    const unlisten = listen<string>('pet:state', () => load());
    return () => {
      cancelled = true;
      unlisten.then((u) => u());
    };
  }, []);

  // Anchor above the ball (same spot as the busy-time status card — status
  // info always lives over the pet's head), then go click-through. show()
  // must come first: on Linux/tao the ignore-events request needs a realized
  // GdkWindow — sent pre-show it is silently skipped.
  useEffect(() => {
    (async () => {
      const self = getCurrentWindow();
      const p = await placeNearBall('above').catch(() => null);
      setPlacement(p);
      await self.show();
      await self.setIgnoreCursorEvents(true).catch(() => {});
    })().catch(() => {});
  }, []);

  const state = status?.state ?? 'idle';
  const activities = status?.activities ?? [];

  return (
    <div className={`pet-detail-window${placement?.side === 'above' ? ' pet-status-above' : ''}`}>
      <div className={`pet-detail pet-detail-${state}`}>
        <div className="pet-detail-state">{STATE_LABEL[state] ?? state}</div>
        {activities.map((a, i) => (
          <div key={i} className="pet-detail-task">
            <span className={`pet-detail-dot pet-detail-dot-${a.state}`} />
            <div className="pet-detail-task-body">
              <div className="pet-detail-title">{a.label}</div>
              {a.detail && <div className="pet-detail-row pet-detail-prompt">{a.detail}</div>}
            </div>
          </div>
        ))}
        <div className="pet-detail-hint">点击打开智能体 · 右键菜单</div>
      </div>
    </div>
  );
}
