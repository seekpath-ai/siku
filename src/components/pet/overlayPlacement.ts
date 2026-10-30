import { getCurrentWindow } from '@tauri-apps/api/window';
import { PhysicalPosition } from '@tauri-apps/api/dpi';
import { monitorFromPoint } from '@tauri-apps/api/window';

export interface BallPlacement {
  /** 'below' by default; 'above' when the ball sits near the screen bottom. */
  side: 'above' | 'below';
  /** Ball-center x relative to the overlay window, in logical px (speech
   *  tail anchor). */
  tailX: number;
}

/** Position an ephemeral overlay window (bubble / menu / detail card)
 *  adjacent to the pet ball: directly below by default, flipped above when
 *  the ball is too close to the screen's bottom edge — the speech-bubble
 *  pattern where the tail always touches the pet. X is left-aligned with the
 *  ball and clamped inside the monitor. */
export async function placeNearBall(): Promise<BallPlacement | null> {
  const { WebviewWindow } = await import('@tauri-apps/api/webviewWindow');
  const self = getCurrentWindow();
  const ball = await WebviewWindow.getByLabel('pet');
  if (!ball) return null;
  const pos = await ball.outerPosition(); // physical px
  const ballSize = await ball.innerSize(); // physical
  const own = await self.innerSize(); // physical
  const scale = await ball.scaleFactor();
  const gap = Math.round(4 * scale);
  const monitor = await monitorFromPoint(
    pos.x + ballSize.width / 2,
    pos.y + ballSize.height / 2
  );

  let side: 'above' | 'below' = 'below';
  if (monitor) {
    const spaceBelow = monitor.position.y + monitor.size.height - (pos.y + ballSize.height);
    const spaceAbove = pos.y - monitor.position.y;
    if (spaceBelow < own.height + gap && spaceAbove > spaceBelow) side = 'above';
  }

  let x = pos.x;
  if (monitor) {
    x = Math.max(
      monitor.position.x + gap,
      Math.min(x, monitor.position.x + monitor.size.width - own.width - gap)
    );
  }
  const y = side === 'below' ? pos.y + ballSize.height + gap : pos.y - own.height - gap;
  await self.setPosition(new PhysicalPosition(x, y));
  return { side, tailX: Math.round((pos.x + ballSize.width / 2 - x) / scale) };
}
