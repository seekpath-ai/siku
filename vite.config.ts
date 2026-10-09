import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'
import { resolve } from 'path'
import { readFileSync } from 'fs'

// Single source of truth for the version shown on the splashscreen.
const tauriConf = JSON.parse(
  readFileSync(resolve(__dirname, 'src-tauri/tauri.conf.json'), 'utf-8')
)

export default defineConfig({
  plugins: [react()],
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
