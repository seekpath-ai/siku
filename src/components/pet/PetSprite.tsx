import { useEffect, useMemo, useState } from 'react';
import type { PetManifest, PetTrack } from '@/lib/tauri';

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
  /** Drag direction while the ball is being moved (null when stationary):
   *  plays running-right / running-left, like codex pets do when walking. */
  moveDir?: 'left' | 'right' | null;
  /** Edge length of the square the sprite is scaled into (window px). */
  boxSize: number;
  /** Sheet failed to decode: parent falls back to the built-in ball. */
  onError?: () => void;
}

/** Spritesheet renderer for `.codex-pet` pets. Frames are row-major sprite
 *  indices over a grid computed from the sheet's natural size ÷ frame size;
 *  a missing state track falls back through `fallback` to `idle`, and
 *  non-loop tracks return to the state track when finished. */
/** Standard codex atlas row → state (v1: rows 0-8; v2 adds look-direction
 *  rows 9-10, unused here). When a manifest omits `animations`, tracks are
 *  derived from this fixed layout. */
const CODEX_ROWS: Record<string, number> = {
  idle: 0,
  'running-right': 1,
  'running-left': 2,
  waving: 3,
  jumping: 4,
  failed: 5,
  waiting: 6,
  running: 7,
  review: 8,
};

export function PetSprite({ manifest, sheetDataUrl, state, interactNonce, moveDir, boxSize, onError }: Props) {
  const [sheetSize, setSheetSize] = useState<{ w: number; h: number } | null>(null);
  const [frameIdx, setFrameIdx] = useState(0);
  const [interacting, setInteracting] = useState<string | null>(null);
  // Tracks derived from the standard atlas layout (blank frames filtered),
  // for manifests that omit `animations`.
  const [autoTracks, setAutoTracks] = useState<Record<string, PetTrack> | null>(null);
  const reducedMotion = useMemo(
    () => window.matchMedia?.('(prefers-reduced-motion: reduce)').matches ?? false,
    []
  );

  const fw = manifest.frame.width || 192;
  const fh = manifest.frame.height || 208;

  useEffect(() => {
    const img = new Image();
    img.onload = () => {
      setSheetSize({ w: img.naturalWidth, h: img.naturalHeight });
      if (Object.keys(manifest.animations).length > 0) return;
      // Derive tracks from the standard codex row layout, dropping fully
      // transparent padding cells (rows are padded to 8 columns; playback
      // uses only the non-empty frames).
      const cols = Math.max(1, Math.floor(img.naturalWidth / fw));
      const rows = Math.max(1, Math.floor(img.naturalHeight / fh));
      const canvas = document.createElement('canvas');
      canvas.width = img.naturalWidth;
      canvas.height = img.naturalHeight;
      const ctx = canvas.getContext('2d');
      if (!ctx) {
        setAutoTracks({ idle: { frames: [0], fps: 8, loop: true } });
        return;
      }
      ctx.drawImage(img, 0, 0);
      const isBlank = (i: number) => {
        const data = ctx.getImageData((i % cols) * fw, Math.floor(i / cols) * fh, fw, fh).data;
        for (let p = 3; p < data.length; p += 16) {
          if (data[p] > 8) return false;
        }
        return true;
      };
      const tracks: Record<string, PetTrack> = {};
      for (const [name, row] of Object.entries(CODEX_ROWS)) {
        if (row >= rows) continue;
        const frames: number[] = [];
        for (let c = 0; c < cols; c++) {
          const idx = row * cols + c;
          if (!isBlank(idx)) frames.push(idx);
        }
        if (frames.length) tracks[name] = { frames, fps: 8, loop: true };
      }
      if (!Object.keys(tracks).length) tracks.idle = { frames: [0], fps: 8, loop: true };
      setAutoTracks(tracks);
    };
    img.onerror = () => onError?.();
    img.src = sheetDataUrl;
  }, [sheetDataUrl, manifest, fw, fh, onError]);

  // Manifests may omit `animations` entirely (the codex norm): tracks then
  // come from the standard row layout derived above.
  const anims = useMemo<Record<string, PetTrack>>(() => {
    if (Object.keys(manifest.animations).length > 0) return manifest.animations;
    return autoTracks ?? manifest.animations;
  }, [manifest, autoTracks]);

  // Click → one interaction track, then back to the state track. Skipped
  // under reduced motion: the animation timer never runs there, so entering
  // `interacting` would stick the sprite on the interaction pose forever.
  useEffect(() => {
    if (!interactNonce || reducedMotion) return;
    const t = INTERACT_TRACKS.find((name) => anims[name]?.frames.length);
    if (!t) return;
    setInteracting(t);
  }, [interactNonce, anims, reducedMotion]);

  const trackName = useMemo(() => {
    if (interacting) return interacting;
    // Being dragged: walk in the drag direction (falls back to the plain
    // running track, then to the state track, when a skin lacks the
    // directional rows).
    if (moveDir) {
      const walk = moveDir === 'left' ? 'running-left' : 'running-right';
      if (anims[walk]?.frames.length) return walk;
      if (anims.running?.frames.length) return 'running';
    }
    const want = STATE_TRACK[state] ?? 'idle';
    if (anims[want]?.frames.length) return want;
    // Track-declared fallback chain, then idle, then first available.
    const fb = anims[want]?.fallback;
    if (fb && anims[fb]?.frames.length) return fb;
    if (anims.idle?.frames.length) return 'idle';
    return Object.keys(anims)[0] ?? null;
  }, [anims, state, interacting, moveDir]);

  const track = trackName ? anims[trackName] : null;
  const cols = sheetSize ? Math.max(1, Math.floor(sheetSize.w / fw)) : 1;

  useEffect(() => {
    if (!track || reducedMotion) {
      setFrameIdx(0);
      // Belt-and-braces: if we somehow got here mid-interaction (e.g. the
      // reduced-motion setting flipped on), release the interaction lock so
      // the state track can show through.
      if (interacting) setInteracting(null);
      return;
    }
    setFrameIdx(0);
    const fps = track.fps > 0 ? track.fps : 8;
    const timer = window.setInterval(() => {
      setFrameIdx((i) => {
        const next = i + 1;
        if (next < track.frames.length) return next;
        // Interaction tracks always play ONCE, even when the track itself
        // says loop — otherwise the sprite is stuck waving/jumping forever
        // and the state track can never come back.
        if (interacting) {
          setInteracting(null);
          return track.frames.length - 1;
        }
        if (track.loop) return 0;
        return track.frames.length - 1;
      });
    }, 1000 / fps);
    return () => window.clearInterval(timer);
  }, [trackName, track, reducedMotion, interacting]);

  if (!track || !sheetSize) return null;

  const sprite = track.frames[Math.min(frameIdx, track.frames.length - 1)] ?? 0;
  const scale = boxSize / fh;
  const x = (sprite % cols) * fw;
  const y = Math.floor(sprite / cols) * fh;

  return (
    <div
      style={{
        position: 'absolute',
        left: 0,
        top: 0,
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
