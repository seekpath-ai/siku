/**
 * Runtime UI theming.
 *
 * All palette colors are CSS variables holding RGB triplets (see :root in
 * index.css and tailwind.config.ts), so a theme switch is just rewriting
 * those variables — no rebuild, no reload. The choice persists in
 * AppSettings (`theme_preset` + `theme_overrides`) and is mirrored to
 * localStorage so the next launch applies it synchronously, before React
 * renders (no flash of the wrong theme).
 *
 * Two writers: the settings page (AppearanceSettings) and the agent's
 * `set_app_theme` tool. Both go through AppSettings; the frontend re-applies
 * on the `app:settings_changed` event the backend emits after every save.
 */
import { listen } from '@tauri-apps/api/event';
import { settingsAppGet, settingsAppSave } from '@/lib/tauri';

function isTauri(): boolean {
  return typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window;
}

export interface ThemePreset {
  id: string;
  name: string;
  /** CSS variable name (without --) → "R G B" triplet. */
  vars: Record<string, string>;
}

/** Default dark — identical to the historical hard-coded palette. */
const DEFAULT_DARK: ThemePreset = {
  id: 'default-dark',
  name: '深色（默认）',
  vars: {
    'background': '26 26 30',
    'surface': '36 36 43',
    'surface-hover': '46 46 54',
    'primary': '230 126 34',
    'accent': '39 174 96',
    'text-primary': '245 245 245',
    'text-secondary': '156 163 175',
    'codex-bg': '13 13 13',
    'codex-surface': '20 20 20',
    'codex-hover': '26 26 26',
    'codex-border': '38 38 38',
    'codex-border-light': '51 51 51',
    'codex-primary': '245 245 245',
    'codex-secondary': '163 163 163',
    'codex-muted': '115 115 115',
    'codex-accent': '25 195 125',
    'codex-accent-hover': '21 167 106',
    'codex-warning': '245 158 11',
    'codex-danger': '239 68 68',
    'codex-code': '10 10 10',
  },
};

const LIGHT: ThemePreset = {
  id: 'light',
  name: '浅色',
  vars: {
    'background': '245 245 244',
    'surface': '255 255 255',
    'surface-hover': '231 229 228',
    'primary': '217 119 6',
    'accent': '22 163 74',
    'text-primary': '28 25 23',
    'text-secondary': '87 83 78',
    'codex-bg': '255 255 255',
    'codex-surface': '247 247 248',
    'codex-hover': '238 238 240',
    'codex-border': '228 228 231',
    'codex-border-light': '212 212 216',
    'codex-primary': '24 24 27',
    'codex-secondary': '82 82 91',
    'codex-muted': '113 113 122',
    'codex-accent': '16 163 127',
    'codex-accent-hover': '13 140 109',
    'codex-warning': '217 119 6',
    'codex-danger': '220 38 38',
    'codex-code': '244 244 245',
    // Inline code: the default pink (#F0ABFC) is unreadable on white.
    'prose-code': '190 24 93',
  },
};

const EYE_CARE: ThemePreset = {
  id: 'eye-care',
  name: '护眼绿',
  vars: {
    'background': '30 38 32',
    'surface': '38 47 40',
    'surface-hover': '47 58 50',
    'primary': '230 126 34',
    'accent': '74 222 128',
    'text-primary': '232 240 233',
    'text-secondary': '160 175 163',
    'codex-bg': '22 29 24',
    'codex-surface': '28 36 30',
    'codex-hover': '34 44 37',
    'codex-border': '44 56 47',
    'codex-border-light': '56 70 59',
    'codex-primary': '228 237 230',
    'codex-secondary': '158 175 161',
    'codex-muted': '110 130 115',
    'codex-accent': '74 222 128',
    'codex-accent-hover': '56 189 110',
    'codex-warning': '245 158 11',
    'codex-danger': '239 68 68',
    'codex-code': '18 24 20',
  },
};

const OCEAN: ThemePreset = {
  id: 'ocean',
  name: '深蓝',
  vars: {
    'background': '15 23 42',
    'surface': '30 41 59',
    'surface-hover': '51 65 85',
    'primary': '230 126 34',
    'accent': '56 189 248',
    'text-primary': '241 245 249',
    'text-secondary': '148 163 184',
    'codex-bg': '11 18 32',
    'codex-surface': '17 26 43',
    'codex-hover': '23 34 54',
    'codex-border': '35 48 72',
    'codex-border-light': '47 62 90',
    'codex-primary': '241 245 249',
    'codex-secondary': '148 163 184',
    'codex-muted': '100 116 139',
    'codex-accent': '56 189 248',
    'codex-accent-hover': '34 165 228',
    'codex-warning': '245 158 11',
    'codex-danger': '239 68 68',
    'codex-code': '8 14 26',
  },
};

export const THEME_PRESETS: ThemePreset[] = [DEFAULT_DARK, LIGHT, EYE_CARE, OCEAN];
export const DEFAULT_THEME_ID = DEFAULT_DARK.id;

/**
 * Semantic override keys (used by AppSettings.theme_overrides and the
 * set_app_theme tool) mapped to the concrete CSS variables they drive.
 * One key can feed both palettes so e.g. "accent" recolors the whole app.
 */
export const THEME_SEMANTIC_VARS: Record<string, string[]> = {
  accent: ['codex-accent', 'accent'],
  accent_hover: ['codex-accent-hover'],
  background: ['codex-bg', 'background'],
  surface: ['codex-surface', 'surface'],
  hover: ['codex-hover', 'surface-hover'],
  border: ['codex-border', 'codex-border-light'],
  text: ['codex-primary', 'text-primary'],
  text_secondary: ['codex-secondary', 'text-secondary'],
  muted: ['codex-muted'],
  primary_brand: ['primary'],
  warning: ['codex-warning'],
  danger: ['codex-danger'],
  code_bg: ['codex-code'],
};

export type ThemeOverrides = Record<string, string>;

const LS_KEY = 'siku-theme';

export function hexToRgbTriplet(hex: string): string | null {
  const m = /^#?([0-9a-fA-F]{6})$/.exec(hex.trim());
  if (!m) return null;
  const n = parseInt(m[1], 16);
  return `${(n >> 16) & 255} ${(n >> 8) & 255} ${n & 255}`;
}

export function getPreset(id: string | null | undefined): ThemePreset {
  return THEME_PRESETS.find((p) => p.id === id) ?? DEFAULT_DARK;
}

/** Every variable any preset or override can set — used to undo the previous
 *  theme fully: an inline var left over from the last preset would otherwise
 *  survive a switch (e.g. light's --prose-code sticking on dark). */
const KNOWN_THEME_VARS: string[] = (() => {
  const set = new Set<string>();
  for (const preset of THEME_PRESETS) {
    for (const key of Object.keys(preset.vars)) set.add(key);
  }
  for (const vars of Object.values(THEME_SEMANTIC_VARS)) {
    for (const v of vars) set.add(v);
  }
  return [...set];
})();

/** Apply preset + overrides to the live document. */
export function applyTheme(presetId: string | null | undefined, overrides?: ThemeOverrides | null): void {
  const preset = getPreset(presetId);
  const root = document.documentElement;
  const next = new Set<string>();
  for (const [name, value] of Object.entries(preset.vars)) {
    root.style.setProperty(`--${name}`, value);
    next.add(name);
  }
  for (const [key, hex] of Object.entries(overrides ?? {})) {
    const triplet = hexToRgbTriplet(hex);
    if (!triplet) continue;
    for (const varName of THEME_SEMANTIC_VARS[key] ?? []) {
      root.style.setProperty(`--${varName}`, triplet);
      next.add(varName);
    }
  }
  for (const name of KNOWN_THEME_VARS) {
    if (!next.has(name)) root.style.removeProperty(`--${name}`);
  }
}

function cacheTheme(presetId: string | null | undefined, overrides?: ThemeOverrides | null): void {
  try {
    localStorage.setItem(LS_KEY, JSON.stringify({ preset: presetId ?? null, overrides: overrides ?? null }));
  } catch { /* private mode etc. — theming just won't survive restart */ }
}

/** Persist the theme choice and let the settings-changed event re-apply it. */
export async function saveTheme(presetId: string | null, overrides: ThemeOverrides | null): Promise<void> {
  cacheTheme(presetId, overrides);
  applyTheme(presetId, overrides);
  if (!isTauri()) return;
  const settings = await settingsAppGet();
  await settingsAppSave({ ...settings, theme_preset: presetId, theme_overrides: overrides });
}

let initialized = false;

/**
 * Apply the cached theme synchronously, then (in Tauri) load the persisted
 * theme and follow future changes. Call once before the first render.
 */
export function initTheme(): void {
  if (initialized) return;
  initialized = true;

  let cached: { preset: string | null; overrides: ThemeOverrides | null } | null = null;
  try {
    cached = JSON.parse(localStorage.getItem(LS_KEY) ?? 'null');
  } catch { /* ignore corrupt cache */ }
  applyTheme(cached?.preset, cached?.overrides);

  if (!isTauri()) return;
  const refresh = () => {
    settingsAppGet()
      .then((s) => {
        cacheTheme(s.theme_preset, s.theme_overrides);
        applyTheme(s.theme_preset, s.theme_overrides);
      })
      .catch(() => { /* backend not ready yet — cached theme stays */ });
  };
  refresh();
  void listen('app:settings_changed', refresh);
}
