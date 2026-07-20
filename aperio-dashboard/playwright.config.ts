import { defineConfig, devices } from '@playwright/test'

// Playwright e2e config for the dashboard. The tests run against a static
// `vite preview` build, so they cover the shell (mount, routing, error/empty
// states) without a live Aperio server. Run with `npm run test:e2e` after a
// one-time `npx playwright install chromium`. Not wired into CI by default —
// full API-backed flows need a running server + backend.
export default defineConfig({
  testDir: './e2e',
  fullyParallel: true,
  forbidOnly: !!process.env.CI,
  retries: process.env.CI ? 2 : 0,
  reporter: 'list',
  use: {
    baseURL: 'http://localhost:4173',
    trace: 'on-first-retry',
  },
  projects: [{ name: 'chromium', use: { ...devices['Desktop Chrome'] } }],
  webServer: {
    command: 'npm run build && npm run preview -- --port 4173',
    url: 'http://localhost:4173',
    reuseExistingServer: !process.env.CI,
    timeout: 120_000,
  },
})
