import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'

export default defineConfig({
  plugins: [react()],
  // host: true so the dev server is reachable from outside a container, not just loopback.
  server: { host: true, port: 5173 },
})
