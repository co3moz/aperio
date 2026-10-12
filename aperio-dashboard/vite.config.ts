import { existsSync, readFileSync } from 'node:fs'
import { fileURLToPath, URL } from 'node:url'
import tailwindcss from '@tailwindcss/vite'
import react from '@vitejs/plugin-react'
import { defineConfig } from 'vite'

// The release this dashboard is built into, so its help links open the
// documentation of that release rather than master's, where a page may since
// have moved. build.rs passes it; a build without it reads the server crate's
// manifest when it is there, and the dev server, like a build with neither
// (the from-source Dockerfile's Node stage), links to master.
function aperioVersion(command: string): string | null {
  if (command === 'serve') return null
  if (process.env.APERIO_VERSION) return process.env.APERIO_VERSION
  const manifest = fileURLToPath(new URL('../aperio-server/Cargo.toml', import.meta.url))
  if (!existsSync(manifest)) return null
  return /^version\s*=\s*"([^"]+)"/m.exec(readFileSync(manifest, 'utf8'))?.[1] ?? null
}

// Built output is embedded into the aperio-server binary and served under the
// /aperio/ prefix. The build is tuned to produce as few files as possible:
// one CSS bundle, no preload polyfill, and small assets inlined as data URIs.
export default defineConfig(({ command }) => ({
  plugins: [react(), tailwindcss()],
  define: {
    __APERIO_VERSION__: JSON.stringify(aperioVersion(command)),
  },
  resolve: {
    alias: {
      '@': fileURLToPath(new URL('./src', import.meta.url)),
    },
  },
  base: '/aperio/',
  build: {
    cssCodeSplit: false,
    // Small assets become data URIs, but font files stay separate so the
    // browser only fetches the unicode-range subsets it actually renders
    // (inlining every Noto Sans subset would balloon the CSS bundle).
    assetsInlineLimit: 8 * 1024,
    modulePreload: { polyfill: false },
    chunkSizeWarningLimit: 1500,
    rollupOptions: {
      input: {
        index: fileURLToPath(new URL('./index.html', import.meta.url)),
        auth: fileURLToPath(new URL('./auth.html', import.meta.url)),
      },
    },
  },
  server: {
    proxy: {
      '/aperio/api': 'http://localhost:8080',
      '/aperio/auth': {
        target: 'http://localhost:8080',
        bypass: (req) => {
          // Serve the login page itself from Vite; proxy only the POST.
          if (req.method === 'GET') return '/aperio/auth.html'
        },
      },
    },
  },
}))
