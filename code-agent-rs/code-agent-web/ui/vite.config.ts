/// <reference types="vitest/config" />
import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'

// https://vite.dev/config/
export default defineConfig({
  plugins: [react()],
  // Build output goes to ../static/ so the Rust server serves it
  build: {
    outDir: '../static',
    emptyOutDir: true,
  },
  // Base path is / since the Rust server serves from root
  base: '/',
  // ── Dev Server ─────────────────────────────────────────
  server: {
    // Proxy API calls to the Rust backend (default :3000)
    proxy: {
      '/health': { target: 'http://localhost:3000', changeOrigin: true },
      '/threads': { target: 'http://localhost:3000', changeOrigin: true },
    },
  },
  // ── Vitest Configuration ──────────────────────────────
  test: {
    environment: 'jsdom',
    globals: true,
    setupFiles: ['./src/test/setup.ts'],
  },
})
