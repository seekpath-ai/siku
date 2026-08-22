import { check } from '@tauri-apps/plugin-updater';
import { ask } from '@tauri-apps/plugin-dialog';
import { relaunch } from '@tauri-apps/plugin-process';

/** Startup update check (main window only). Pulls the latest release manifest
 *  from the endpoint configured in tauri.conf.json (GitHub Releases) and, on
 *  user confirmation, downloads, verifies the minisign signature, installs,
 *  and relaunches. Silent on any failure — an update check must never break
 *  app startup (offline, dev build, endpoint unreachable). */
export async function checkForUpdatesOnStartup(): Promise<void> {
  if (import.meta.env.DEV) return;
  try {
    const update = await check();
    if (!update) return;
    const yes = await ask(
      `发现新版本 v${update.version}（当前 v${update.currentVersion}），是否立即更新？`,
      { title: '应用更新', kind: 'info', okLabel: '立即更新', cancelLabel: '稍后' },
    );
    if (!yes) return;
    await update.downloadAndInstall();
    await relaunch();
  } catch {
    // best-effort
  }
}
