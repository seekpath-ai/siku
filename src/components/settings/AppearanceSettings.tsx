import { useEffect, useState } from 'react';
import { Check, RotateCcw } from 'lucide-react';
import { settingsAppGet } from '@/lib/tauri';
import {
  DEFAULT_THEME_ID,
  THEME_PRESETS,
  applyTheme,
  saveTheme,
  type ThemeOverrides,
} from '@/lib/theme';

/** Darken a #rrggbb hex by `ratio` (0–1) for the hover-state companion. */
function darken(hex: string, ratio: number): string {
  const n = parseInt(hex.replace('#', ''), 16);
  const ch = (v: number) => Math.round(v * (1 - ratio)).toString(16).padStart(2, '0');
  return `#${ch((n >> 16) & 255)}${ch((n >> 8) & 255)}${ch(n & 255)}`;
}

/** Settings page section: UI theme preset + accent color override. */
export function AppearanceSettings() {
  const [presetId, setPresetId] = useState<string>(DEFAULT_THEME_ID);
  const [overrides, setOverrides] = useState<ThemeOverrides>({});
  const [loaded, setLoaded] = useState(false);

  useEffect(() => {
    settingsAppGet()
      .then((s) => {
        setPresetId(s.theme_preset ?? DEFAULT_THEME_ID);
        setOverrides(s.theme_overrides ?? {});
      })
      .catch(() => {})
      .finally(() => setLoaded(true));
  }, []);

  const persist = (nextPreset: string, nextOverrides: ThemeOverrides) => {
    setPresetId(nextPreset);
    setOverrides(nextOverrides);
    void saveTheme(
      nextPreset === DEFAULT_THEME_ID && Object.keys(nextOverrides).length === 0 ? null : nextPreset,
      Object.keys(nextOverrides).length === 0 ? null : nextOverrides,
    );
  };

  const setAccent = (hex: string) => {
    persist(presetId, { ...overrides, accent: hex, accent_hover: darken(hex, 0.15) });
  };

  const resetAll = () => persist(DEFAULT_THEME_ID, {});

  if (!loaded) return null;
  const isDefault = presetId === DEFAULT_THEME_ID && Object.keys(overrides).length === 0;

  return (
    <div className="space-y-8">
      <section>
        <h2 className="text-base font-medium text-text-primary mb-1">主题</h2>
        <p className="text-[12px] text-text-secondary mb-4">
          立即生效，无需重启。也可以在对话里直接说「换成护眼绿」「强调色改成橙色」。
        </p>
        <div className="grid grid-cols-2 gap-3">
          {THEME_PRESETS.map((preset) => {
            const selected = preset.id === presetId;
            return (
              <button
                key={preset.id}
                onClick={() => persist(preset.id, overrides)}
                onMouseEnter={() => applyTheme(preset.id, overrides)}
                onMouseLeave={() => applyTheme(presetId, overrides)}
                className={`flex items-center gap-3 rounded-lg border p-3 text-left transition-colors ${
                  selected
                    ? 'border-primary bg-surface'
                    : 'border-surface-hover bg-surface hover:border-text-secondary'
                }`}
              >
                <span
                  className="flex h-9 w-14 shrink-0 overflow-hidden rounded border border-surface-hover"
                  aria-hidden
                >
                  <span className="h-full w-1/3" style={{ background: `rgb(${preset.vars['codex-bg']})` }} />
                  <span className="h-full w-1/3" style={{ background: `rgb(${preset.vars['codex-surface']})` }} />
                  <span className="h-full w-1/3" style={{ background: `rgb(${preset.vars['codex-accent']})` }} />
                </span>
                <span className="flex-1 text-[13px] text-text-primary">{preset.name}</span>
                {selected && <Check size={14} className="text-primary" />}
              </button>
            );
          })}
        </div>
      </section>

      <section>
        <h2 className="text-base font-medium text-text-primary mb-1">强调色</h2>
        <p className="text-[12px] text-text-secondary mb-3">
          覆盖预设中的主强调色（按钮、链接、选中态）。
        </p>
        <div className="flex items-center gap-3">
          <input
            type="color"
            value={overrides.accent ?? '#19c37d'}
            onChange={(e) => setAccent(e.target.value)}
            className="h-8 w-12 cursor-pointer rounded border border-surface-hover bg-surface p-0.5"
          />
          <span className="text-[12px] text-text-secondary">{overrides.accent ?? '跟随主题'}</span>
          {overrides.accent && (
            <button
              onClick={() => {
                const next = { ...overrides };
                delete next.accent;
                delete next.accent_hover;
                persist(presetId, next);
              }}
              className="text-[12px] text-text-secondary hover:text-text-primary"
            >
              清除
            </button>
          )}
        </div>
      </section>

      <section>
        <button
          onClick={resetAll}
          disabled={isDefault}
          className="flex items-center gap-2 rounded-lg border border-surface-hover bg-surface px-3 py-2 text-[13px] text-text-secondary transition-colors hover:text-text-primary disabled:opacity-40"
        >
          <RotateCcw size={14} />
          恢复默认主题
        </button>
      </section>
    </div>
  );
}
