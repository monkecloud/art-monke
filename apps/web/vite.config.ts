import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'

export default defineConfig({
  plugins: [react()],
  server: {
    // host: true so the dev server is reachable from outside a container, not just loopback.
    host: true,
    port: 5173,
    // Stands in for the Ingress: in the cluster /api is the same origin, served by the api
    // pods. Without this the dev server answers /api itself with index.html and every
    // fetch parses HTML as JSON. The path is not rewritten, matching Traefik.
    proxy: { '/api': 'http://localhost:8080' },
  },
})
