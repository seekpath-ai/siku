import { useEffect } from 'react';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { PhysicalPosition } from '@tauri-apps/api/dpi';
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
 *  speech bubble: own always-on-top transparent window so the 48×48 ball
 *  window never resizes; click-through so it never steals pointer events. */
export function PetDetailWindow() {
  const q = new URLSearchParams(window.location.search);
  const state = q.get('state') ?? 'idle';
  const title = q.get('title') ?? '';
  const prompt = q.get('prompt') ?? '';
  const tool = q.get('tool') ?? '';
  const pending = q.get('pending') ?? '';

  // Anchor below the ball and go click-through before becoming visible.
  useEffect(() => {
    (async () => {
      const { WebviewWindow } = await import('@tauri-apps/api/webviewWindow');
      const self = getCurrentWindow();
      const ball = await WebviewWindow.getByLabel('pet');
      if (ball) {
        const pos = await ball.outerPosition();
        const scale = await ball.scaleFactor();
        await self.setPosition(new PhysicalPosition(pos.x, pos.y + Math.round(52 * scale)));
      }
      await self.setIgnoreCursorEvents(true).catch(() => {});
      await self.show();
    })().catch(() => {});
  }, []);

  return (
    <div className="pet-detail-window">
      <div className={`pet-detail pet-detail-${state}`}>
        <div className="pet-detail-state">{STATE_LABEL[state] ?? state}</div>
        {title && <div className="pet-detail-title">{title}</div>}
        {pending && <div className="pet-detail-pending">待处理：{pending}</div>}
        {tool && state === 'running' && <div className="pet-detail-row">工具：{tool}</div>}
        {prompt && <div className="pet-detail-row pet-detail-prompt">{prompt}</div>}
      </div>
    </div>
  );
}
