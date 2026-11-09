import {
  listen as tauriListen,
  emit as tauriEmit,
  type EventCallback,
  type EventName,
  type UnlistenFn,
} from '@tauri-apps/api/event';

/**
 * Tauri event API 的浏览器安全封装。
 *
 * 纯浏览器环境（E2E / vite 预览）没有 __TAURI_INTERNALS__，裸 listen/emit
 * 会抛 TypeError 并在控制台留下噪音（E2E 的「页面加载无异常」断言即因此
 * 失败）。封装后在浏览器中静默降级为 no-op；Tauri 窗口内行为不变。
 */

function hasTauri(): boolean {
  return typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window;
}

export function listen<T>(event: EventName, handler: EventCallback<T>): Promise<UnlistenFn> {
  if (!hasTauri()) return Promise.resolve(() => {});
  return tauriListen<T>(event, handler);
}

export function emit(event: string, payload?: unknown): Promise<void> {
  if (!hasTauri()) return Promise.resolve();
  return tauriEmit(event, payload);
}

export type { UnlistenFn };
