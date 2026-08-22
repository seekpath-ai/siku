import type { Config } from 'tailwindcss'
import typography from '@tailwindcss/typography'

export default {
  content: ['./index.html', './src/**/*.{js,ts,jsx,tsx}'],
  theme: {
    extend: {
      colors: {
        background: '#1A1A1E',
        surface: '#24242B',
        'surface-hover': '#2E2E36',
        primary: '#E67E22',
        accent: '#27AE60',
        'text-primary': '#F5F5F5',
        'text-secondary': '#9CA3AF',
        codex: {
          bg: '#0d0d0d',
          surface: '#141414',
          hover: '#1a1a1a',
          border: '#262626',
          'border-light': '#333333',
          primary: '#f5f5f5',
          secondary: '#a3a3a3',
          muted: '#737373',
          accent: '#19c37d',
          'accent-hover': '#15a76a',
          warning: '#f59e0b',
          danger: '#ef4444',
          code: '#0a0a0a',
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
