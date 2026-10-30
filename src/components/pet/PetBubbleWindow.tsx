import { useEffect } from 'react';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { PhysicalPosition } from '@tauri-apps/api/dpi';
import './pet-window.css';

const BUBBLE_MS = 3200;

/** Ephemeral click-through speech bubble shown below the pet ball. Lives in
 *  its own always-on-top transparent window (created by PetBallWindow on
 *  `pet:bubble`) so the ball window itself never has to resize — a resized
 *  transparent window keeps swallowing mouse events over the enlarged,
 *  invisible area. Positions itself under the ball, goes click-through,
 *  shows, and closes itself after a few seconds. */
export function PetBubbleWindow() {
  const text = new URLSearchParams(window.location.search).get('text') ?? '';

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
        // Place the bubble just below the ball.
        await self.setPosition(
          new PhysicalPosition(pos.x, pos.y + ballSize.height + Math.round(4 * scale))
        );
      }
      await self.setIgnoreCursorEvents(true).catch(() => {});
      await self.show();
    })().catch(() => {});
  }, []);

  // Self-destruct; PetBallWindow also closes any stale bubble window before
  // spawning a new one, and a leftover bubble is harmless anyway (invisible,
  // click-through).
  useEffect(() => {
    const timer = window.setTimeout(() => {
      getCurrentWindow().close().catch(() => {});
    }, BUBBLE_MS);
    return () => window.clearTimeout(timer);
  }, []);

  if (!text) return null;
  return (
    <div className="pet-bubble pet-bubble-window">
      <span>{text}</span>
      <div className="pet-bubble-arrow" />
    </div>
  );
}
