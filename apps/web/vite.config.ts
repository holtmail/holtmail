import { fileURLToPath } from 'node:url'

import tailwindcss from '@tailwindcss/vite'
import react from '@vitejs/plugin-react'
import { defineConfig } from 'vite'

const server = process.env.HOLTMAIL_SERVER ?? 'http://127.0.0.1:8080'

export default defineConfig({
  root: fileURLToPath(new URL('.', import.meta.url)),
  plugins: [react(), tailwindcss()],
  server: {
    proxy: {
      '/api': { target: server, changeOrigin: false },
    },
  },
  build: {
    outDir: fileURLToPath(new URL('dist', import.meta.url)),
    emptyOutDir: true,
    chunkSizeWarningLimit: 700,
  },
})
