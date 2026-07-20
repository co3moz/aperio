import { fileURLToPath, URL } from 'node:url'
import { defineConfig } from 'vitest/config'

// Vitest scans only `src` for unit tests, so the Playwright specs under `e2e/`
// (which import @playwright/test and run under a different runner) are never
// picked up here. The `@` alias mirrors vite.config.ts.
export default defineConfig({
  resolve: {
    alias: { '@': fileURLToPath(new URL('./src', import.meta.url)) },
  },
  test: {
    include: ['src/**/*.{test,spec}.{ts,tsx}'],
    environment: 'node',
  },
})
