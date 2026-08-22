/** Reader page themes: recolor the rendered PDF canvas (duotone luminance
 *  map, applied by PdfViewer after rendering). Presets mirror Zotero's
 *  reader themes; users can add custom ones. */

export interface ReaderTheme {
  id: string;
  name: string;
  /** Empty string = keep the PDF's original colors (no recoloring). */
  background: string;
  foreground: string;
  custom?: boolean;
}

export const ORIGINAL_THEME: ReaderTheme = {
  id: 'original',
  name: '原始',
  background: '',
  foreground: '',
};

export const PRESET_THEMES: ReaderTheme[] = [
  ORIGINAL_THEME,
  { id: 'dark', name: '深色', background: '#2E3440', foreground: '#D8DEE9' },
  { id: 'black', name: '黑色', background: '#000000', foreground: '#FFFFFF' },
  { id: 'snow', name: '雪色', background: '#FAFAFA', foreground: '#24292F' },
  { id: 'sepia', name: '棕褐', background: '#F4ECD8', foreground: '#5B4636' },
];

const CUSTOM_KEY = 'siku.reader.themes.custom';
const SELECTED_KEY = 'siku.reader.theme.selected';

export function loadCustomThemes(): ReaderTheme[] {
  try {
    const raw = localStorage.getItem(CUSTOM_KEY);
    if (!raw) return [];
    const list = JSON.parse(raw) as ReaderTheme[];
    if (!Array.isArray(list)) return [];
    return list.filter((t) => t && t.id && t.background && t.foreground);
  } catch {
    return [];
  }
}

export function saveCustomThemes(themes: ReaderTheme[]): void {
  try {
    localStorage.setItem(CUSTOM_KEY, JSON.stringify(themes));
  } catch { /* ignore storage errors */ }
}

export function loadSelectedThemeId(): string {
  try {
    return localStorage.getItem(SELECTED_KEY) || ORIGINAL_THEME.id;
  } catch {
    return ORIGINAL_THEME.id;
  }
}

export function saveSelectedThemeId(id: string): void {
  try {
    localStorage.setItem(SELECTED_KEY, id);
  } catch { /* ignore storage errors */ }
}

/** The background/foreground color pair for a theme, or null for
 *  "original" (no recoloring). Consumed by PdfViewer's canvas recolor. */
export function themeToPageColors(
  theme: ReaderTheme
): { background: string; foreground: string } | null {
  if (!theme.background || !theme.foreground) return null;
  return { background: theme.background, foreground: theme.foreground };
}
