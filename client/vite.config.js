import { defineConfig, loadEnv } from 'vite'
import react from '@vitejs/plugin-react'
import process from 'node:process'

// https://vite.dev/config/
export default defineConfig(({ mode }) => {
  const env = loadEnv(mode, process.cwd(), '')
  const serverUrl = env.VITE_SERVER_URL || 'http://localhost:3001'
  
  return {
    plugins: [react()],
    server: {
      proxy: {
        '/api': {
          target: serverUrl,
          changeOrigin: true,
        },
        '/room-count': {
          target: serverUrl,
          changeOrigin: true,
        },
        '/list-rooms': {
          target: serverUrl,
          changeOrigin: true,
        },
        '/quick-join': {
          target: serverUrl,
          changeOrigin: true,
        },
        '/img': {
          target: serverUrl,
          changeOrigin: true,
        },
      },
    },
  }
})
