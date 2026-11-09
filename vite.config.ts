import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'
import { resolve } from 'path'
import { readFileSync } from 'fs'

// Single source of truth for the version shown on the splashscreen.
const tauriConf = JSON.parse(
  readFileSync(resolve(__dirname, 'src-tauri/tauri.conf.json'), 'utf-8')
)

export default defineConfig({
  plugins: [
    react(),
    {
      // KaTeX 字体三格式（ttf/woff/woff2）全量入包会让 dist 多出数 MB；
      // 现代 webview 全部支持 woff2，把 @font-face 的 src 裁剪到只剩 woff2。
      name: 'katex-woff2-only',
      enforce: 'pre' as const,
      transform(code: string, id: string) {
        if (!/katex(?:\.min)?\.css/.test(id)) return null;
        return code.replace(/src:[^}]+/g, (src: string) => {
          const woff2 = src.match(/url\([^)]*\.woff2\)\s*format\(["']woff2["']\)/);
          return woff2 ? `src:${woff2[0]}` : src;
        });
      },
    },
  ],
  define: {
    __APP_VERSION__: JSON.stringify(tauriConf.version),
  },
  resolve: {
    alias: {
      '@': resolve(__dirname, './src'),
    },
  },
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    host: true,
    watch: {
      ignored: ['**/src-tauri/**'],
    },
  },
  build: {
    target: process.env.TAURI_ENV_PLATFORM === 'windows' ? 'chrome105' : 'safari15',
    minify: !process.env.TAURI_ENV_DEBUG ? 'esbuild' : false,
    sourcemap: !!process.env.TAURI_ENV_DEBUG,
    outDir: 'dist',
    rollupOptions: {
      input: {
        main: resolve(__dirname, 'index.html'),
        splashscreen: resolve(__dirname, 'splashscreen.html'),
      },
    },
  },
})
