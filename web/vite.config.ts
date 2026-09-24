import tailwindcss from '@tailwindcss/vite'
import react from '@vitejs/plugin-react'
import { defineConfig } from 'vitest/config'

// The server embeds `dist` at compile time: `index.html` at `/`, hashed
// files under `/assets/`, and `public/` files at the root.
export default defineConfig({
  plugins: [react(), tailwindcss()],
  build: {
    outDir: 'dist',
    assetsDir: 'assets',
    emptyOutDir: true,
  },
  server: {
    // `npm run dev` talks to a Rustorr on localhost:8090.
    proxy: Object.fromEntries(
      [
        '/echo', '/torrents', '/torrent', '/settings', '/viewed', '/cache', '/stream', '/play',
        '/playlist', '/playlistall', '/search', '/torznab', '/storage', '/tmdb', '/waf', '/gst',
        '/ffp', '/magnets', '/shutdown',
      ].map((path) => [path, 'http://localhost:8090']),
    ),
  },
  test: {
    environment: 'jsdom',
    include: ['src/**/*.test.{ts,tsx}'],
  },
})
