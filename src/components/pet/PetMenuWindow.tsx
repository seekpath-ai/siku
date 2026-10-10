import { useEffect } from 'react';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { PhysicalPosition } from '@tauri-apps/api/dpi';
import { EyeOff } from 'lucide-react';
import { settingsAppGet, settingsAppSave } from '@/lib/tauri';
import './pet-window.css';

/** The pet ball's right-click menu as its OWN ephemeral window. The ball
 *  window never resizes: a resized transparent window keeps swallowing mouse
 *  events over its enlarged invisible area (clicks right of the ball opened
 *  the panel), and the old in-window "click outside" listener could never
 *  fire for clicks on the desktop — leaving the oversized hit region stuck.
 *  This window closes on blur, which is exactly how a context menu should
 *  behave. */
export function PetMenuWindow() {
  // Anchor below the ball, then show focused (needed for blur/Esc semantics).
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
      await self.show();
      await self.setFocus();
    })().catch(() => {});
  }, []);

  // A context menu dies when it loses focus (click anywhere else) or on Esc.
  useEffect(() => {
    const self = getCurrentWindow();
    let unlisten: (() => void) | undefined;
    self
      .onFocusChanged(({ payload: focused }) => {
        if (!focused) self.close().catch(() => {});
      })
      .then((fn) => {
        unlisten = fn;
      })
      .catch(() => {});
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') self.close().catch(() => {});
    };
    window.addEventListener('keydown', onKey);
    return () => {
      unlisten?.();
      window.removeEventListener('keydown', onKey);
    };
  }, []);

  const handleHide = async () => {
    try {
      const current = await settingsAppGet();
      // settings_app_save broadcasts app:settings_changed; the main window
      // reacts by closing the ball window.
      await settingsAppSave({ ...current, show_pet: false });
    } catch (err) {
      console.error('Failed to update pet setting:', err);
    }
    getCurrentWindow().close().catch(() => {});
  };

  return (
    <div className="p-1">
      <div className="min-w-[140px] py-1 bg-surface border border-surface-hover rounded-lg shadow-xl">
        <button
          onClick={handleHide}
          className="w-full flex items-center gap-2 px-3 py-2 text-xs text-text-primary hover:bg-surface-hover transition-colors"
        >
          <EyeOff size={13} className="text-text-secondary" />
          隐藏宠物球
        </button>
      </div>
    </div>
  );
}
