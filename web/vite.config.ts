import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'

// 开发态：前端 5173，API 与 WebSocket 全部代理到平台 8000。
// 生产态：npm run build 产出 web/dist，由平台静态托管（单端口交付）。
export default defineConfig({
  plugins: [react()],
  server: {
    port: 5173,
    host: true,
    proxy: {
      '/api': {
        target: 'http://127.0.0.1:8000',
        changeOrigin: true,
      },
      '/ws': {
        target: 'ws://127.0.0.1:8000',
        ws: true,
        changeOrigin: true,
      },
    },
  },
  build: {
    outDir: 'dist',
    sourcemap: false,
    chunkSizeWarningLimit: 1500,
  },
})
