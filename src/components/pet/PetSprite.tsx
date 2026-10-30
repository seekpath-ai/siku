import { useEffect, useMemo, useState } from 'react';
import type { PetManifest } from '@/lib/tauri';

/** Agent states the ball mirrors → codex pet track names. */
const STATE_TRACK: Record<string, string> = {
  idle: 'idle',
  running: 'running',
  waiting: 'waiting',
  done: 'review',
  failed: 'failed',
};

/** Interaction tracks tried in order on click (codex convention). */
const INTERACT_TRACKS = ['wave', 'bounce', 'jumping', 'waving'];

interface Props {
  manifest: PetManifest;
  sheetDataUrl: string;
  state: 'idle' | 'running' | 'waiting' | 'done' | 'failed';
  /** Bumped on click: play the first interaction track once. */
  interactNonce: number;
}

/** Spritesheet renderer for `.codex-pet` pets. Frames are row-major sprite
 *  indices over a grid computed from the sheet's natural size ÷ frame size;
 *  a missing state track falls back through `fallback` to `idle`, and
 *  non-loop tracks return to the state track when finished. */
export function PetSprite({ manifest, sheetDataUrl, state, interactNonce }: Props) {
  const [sheetSize, setSheetSize] = useState<{ w: number; h: number } | null>(null);
  const [frameIdx, setFrameIdx] = useState(0);
  const [interacting, setInteracting] = useState<string | null>(null);
  const reducedMotion = useMemo(
    () => window.matchMedia?.('(prefers-reduced-motion: reduce)').matches ?? false,
    []
  );

  const fw = manifest.frame.width || 192;
  const fh = manifest.frame.height || 208;

  useEffect(() => {
    const img = new Image();
    img.onload = () => setSheetSize({ w: img.naturalWidth, h: img.naturalHeight });
    img.src = sheetDataUrl;
  }, [sheetDataUrl]);

  // Click → one interaction track, then back to the state track.
  useEffect(() => {
    if (!interactNonce) return;
    const t = INTERACT_TRACKS.find((name) => manifest.animations[name]?.frames.length);
    if (!t) return;
    setInteracting(t);
  }, [interactNonce, manifest]);

  const trackName = useMemo(() => {
    if (interacting) return interacting;
    const want = STATE_TRACK[state] ?? 'idle';
    const anims = manifest.animations;
    if (anims[want]?.frames.length) return want;
    // Track-declared fallback chain, then idle, then first available.
    const fb = anims[want]?.fallback;
    if (fb && anims[fb]?.frames.length) return fb;
    if (anims.idle?.frames.length) return 'idle';
    return Object.keys(anims)[0] ?? null;
  }, [manifest, state, interacting]);

  const track = trackName ? manifest.animations[trackName] : null;
  const cols = sheetSize ? Math.max(1, Math.floor(sheetSize.w / fw)) : 1;

  useEffect(() => {
    if (!track || reducedMotion) {
      setFrameIdx(0);
      return;
    }
    setFrameIdx(0);
    const fps = track.fps > 0 ? track.fps : 8;
    const timer = window.setInterval(() => {
      setFrameIdx((i) => {
        const next = i + 1;
        if (next < track.frames.length) return next;
        if (track.loop) return 0;
        // One-shot finished: back to the state track.
        if (interacting) setInteracting(null);
        return track.frames.length - 1;
      });
    }, 1000 / fps);
    return () => window.clearInterval(timer);
  }, [trackName, track, reducedMotion, interacting]);

  if (!track || !sheetSize) return null;

  const sprite = track.frames[Math.min(frameIdx, track.frames.length - 1)] ?? 0;
  // Sprite box scaled to fit the 48px ball window (44px box).
  const scale = 44 / fh;
  const x = (sprite % cols) * fw;
  const y = Math.floor(sprite / cols) * fh;

  return (
    <div
      style={{
        position: 'absolute',
        left: 2,
        top: 2,
        width: fw * scale,
        height: fh * scale,
        backgroundImage: `url(${sheetDataUrl})`,
        backgroundSize: `${sheetSize.w * scale}px ${sheetSize.h * scale}px`,
        backgroundPosition: `-${x * scale}px -${y * scale}px`,
        imageRendering: 'auto',
        pointerEvents: 'none',
      }}
    />
  );
}
