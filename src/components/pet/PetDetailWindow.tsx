import { useEffect, useState } from 'react';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { PhysicalPosition } from '@tauri-apps/api/dpi';
import { listen } from '@tauri-apps/api/event';
import { petCurrentStatus } from '@/lib/tauri';
import type { PetStatusInfo } from '@/lib/tauri';
import './pet-window.css';

const STATE_LABEL: Record<string, string> = {
  running: '思考 / 工作中',
  waiting: '等待确认',
  done: '已完成',
  failed: '出错了',
  idle: '空闲',
};

/** Ephemeral hover detail card shown below the pet ball (created by
 *  PetBallWindow on mouse-enter, closed on mouse-leave). Same pattern as the
 *  speech bubble: own always-on-top transparent window so the ball window
 *  stays exactly ball-sized; click-through so it never steals pointer events.
 *  Fetches the status itself (supporting multiple concurrent tasks) and
 *  refreshes whenever the aggregate state changes. */
export function PetDetailWindow() {
  const [status, setStatus] = useState<PetStatusInfo | null>(null);

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

  // Anchor below the ball and go click-through before becoming visible.
  useEffect(() => {
    (async () => {
      const { WebviewWindow } = await import('@tauri-apps/api/webviewWindow');
      const self = getCurrentWindow();
      const ball = await WebviewWindow.getByLabel('pet');
      if (ball) {
        const pos = await ball.outerPosition();
        const ballSize = await ball.innerSize();
        const scale = await ball.scaleFactor();
        await self.setPosition(
          new PhysicalPosition(pos.x, pos.y + ballSize.height + Math.round(4 * scale))
        );
      }
      await self.setIgnoreCursorEvents(true).catch(() => {});
      await self.show();
    })().catch(() => {});
  }, []);

  const state = status?.state ?? 'idle';
  const activities = status?.activities ?? [];

  return (
    <div className="pet-detail-window">
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
