export async function pickDirectory(current?: string): Promise<string | null> {
  try {
    const { open } = await import('@tauri-apps/plugin-dialog');
    const selected = await open({ directory: true, defaultPath: current || undefined });
    if (typeof selected === 'string') return selected;
  } catch {
    // Fallback when running outside Tauri (e.g. browser dev / tests)
  }
  return null;
}
