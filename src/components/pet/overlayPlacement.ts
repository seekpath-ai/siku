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

/** The overlay's own size in LOGICAL px (DPI-invariant), derived from its
 *  physical innerSize ÷ the scale factor of the monitor it currently sits on.
 *  Needed because the window may spawn on a high-DPI primary monitor while
 *  the ball lives on a different-scale monitor — raw physical sizes measured
 *  on the wrong monitor misplace the window ("card floats too high"). */
async function ownLogicalSize(self: ReturnType<typeof getCurrentWindow>): Promise<{ w: number; h: number }> {
  const phys = await self.innerSize();
  const scale = await self.scaleFactor();
  return { w: phys.width / scale, h: phys.height / scale };
}

/** Position an ephemeral overlay window (bubble / menu / detail card)
 *  adjacent to the pet ball. `prefer` = 'below' (speech-bubble default, tail
 *  at the ball) or 'above' (Codex-style status card over the pet's head);
 *  either flips when the preferred side lacks room. X is left-aligned with
 *  the ball and clamped inside the monitor. */
export async function placeNearBall(prefer: 'above' | 'below' = 'below'): Promise<BallPlacement | null> {
  const { WebviewWindow } = await import('@tauri-apps/api/webviewWindow');
  const self = getCurrentWindow();
  const ball = await WebviewWindow.getByLabel('pet');
  if (!ball) return null;
  const pos = await ball.outerPosition(); // physical px
  const ballSize = await ball.innerSize(); // physical
  const scale = await ball.scaleFactor();
  const monitor = await monitorFromPoint(
    pos.x + ballSize.width / 2,
    pos.y + ballSize.height / 2
  );
  // Convert the overlay's size into the TARGET monitor's physical space:
  // logical size is DPI-invariant, monitor rects are physical.
  const ownLogical = await ownLogicalSize(self);
  const tScale = monitor?.scaleFactor ?? scale;
  const own = { width: ownLogical.w * tScale, height: ownLogical.h * tScale };
  const gap = Math.round(4 * tScale);

  let side: 'above' | 'below' = prefer;
  if (monitor) {
    const needed = own.height + gap;
    const spaceBelow = monitor.position.y + monitor.size.height - (pos.y + ballSize.height);
    const spaceAbove = pos.y - monitor.position.y;
    if (side === 'below' && spaceBelow < needed && spaceAbove > spaceBelow) side = 'above';
    else if (side === 'above' && spaceAbove < needed && spaceBelow > spaceAbove) side = 'below';
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

/** Position an overlay window at an arbitrary screen point (physical px) —
 *  e.g. the right-click cursor for the pet menu, Codex-style. Flips above
 *  the point near the monitor's bottom edge and clamps inside the monitor. */
export async function placeAtPoint(px: number, py: number): Promise<void> {
  const self = getCurrentWindow();
  const monitor = await monitorFromPoint(px, py);
  const ownLogical = await ownLogicalSize(self);
  const tScale = monitor?.scaleFactor ?? 1;
  const own = { width: ownLogical.w * tScale, height: ownLogical.h * tScale };
  const gap = Math.round(4 * tScale);
  let x = px;
  let y = py;
  if (monitor) {
    if (py + own.height > monitor.position.y + monitor.size.height - gap) {
      y = py - own.height;
    }
    x = Math.max(
      monitor.position.x + gap,
      Math.min(x, monitor.position.x + monitor.size.width - own.width - gap)
    );
    y = Math.max(
      monitor.position.y + gap,
      Math.min(y, monitor.position.y + monitor.size.height - own.height - gap)
    );
  }
  await self.setPosition(new PhysicalPosition(x, y));
}
