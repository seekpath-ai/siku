import type { Config } from 'tailwindcss'
import typography from '@tailwindcss/typography'

// Colors are CSS variables (RGB triplets, e.g. `--codex-accent: 25 195 125`)
// so the UI theme can be switched at runtime — see src/lib/theme.ts. The
// `<alpha-value>` placeholder keeps opacity modifiers (`bg-codex-accent/15`)
// working. Default values live in src/index.css `:root`.
const v = (name: string) => `rgb(var(${name}) / <alpha-value>)`

export default {
  content: ['./index.html', './src/**/*.{js,ts,jsx,tsx}'],
  theme: {
    extend: {
      colors: {
        background: v('--background'),
        surface: v('--surface'),
        'surface-hover': v('--surface-hover'),
        primary: v('--primary'),
        accent: v('--accent'),
        'text-primary': v('--text-primary'),
        'text-secondary': v('--text-secondary'),
        codex: {
          bg: v('--codex-bg'),
          surface: v('--codex-surface'),
          hover: v('--codex-hover'),
          border: v('--codex-border'),
          'border-light': v('--codex-border-light'),
          primary: v('--codex-primary'),
          secondary: v('--codex-secondary'),
          muted: v('--codex-muted'),
          accent: v('--codex-accent'),
          'accent-hover': v('--codex-accent-hover'),
          warning: v('--codex-warning'),
          danger: v('--codex-danger'),
          code: v('--codex-code'),
        },
      },
      fontFamily: {
        sans: ['Inter', 'PingFang SC', 'Noto Sans SC', 'sans-serif'],
        serif: ['Times New Roman', 'Noto Serif SC', 'serif'],
      },
    },
  },
  plugins: [typography()],
} satisfies Config
