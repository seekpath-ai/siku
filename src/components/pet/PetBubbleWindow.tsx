import { useEffect, useState } from 'react';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { placeNearBall } from './overlayPlacement';
import type { BallPlacement } from './overlayPlacement';
import './pet-window.css';

const BUBBLE_MS = 3200;

/** Ephemeral click-through speech bubble anchored to the pet ball like the
 *  pet is talking: the tail points at the ball, and the whole bubble flips
 *  above the ball when there is no room below (ball near the screen bottom).
 *  Lives in its own always-on-top transparent window (created by
 *  PetBallWindow on `pet:bubble`) so the ball window itself never has to
 *  resize — a resized transparent window keeps swallowing mouse events over
 *  the enlarged, invisible area. */
export function PetBubbleWindow() {
  const text = new URLSearchParams(window.location.search).get('text') ?? '';
  const [placement, setPlacement] = useState<BallPlacement | null>(null);

  // Anchor next to the ball and go click-through. show() must come first: on
  // Linux/tao the ignore-events request needs a realized GdkWindow — sent
  // pre-show it is silently skipped and the bubble would swallow clicks.
  useEffect(() => {
    (async () => {
      const self = getCurrentWindow();
      const p = await placeNearBall().catch(() => null);
      setPlacement(p);
      await self.show();
      await self.setIgnoreCursorEvents(true).catch(() => {});
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
  const above = placement?.side === 'above';
  // Tail points at the ball's center, clamped inside the bubble.
  const tailX = Math.max(12, Math.min(placement?.tailX ?? 20, 248));
  return (
    <div className={`pet-bubble pet-bubble-window${above ? ' pet-bubble-above' : ''}`}>
      {!above && <div className="pet-bubble-arrow" style={{ left: tailX - 4 }} />}
      <span>{text}</span>
      {above && <div className="pet-bubble-arrow pet-bubble-arrow-down" style={{ left: tailX - 4 }} />}
    </div>
  );
}
