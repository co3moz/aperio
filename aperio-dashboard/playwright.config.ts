import { defineConfig, devices } from '@playwright/test'

// Playwright e2e config for the dashboard. The tests run against a static
// `vite preview` build, so they cover the shell (mount, routing, error/empty
// states) without a live Aperio server. Run with `npm run test:e2e` after a
// one-time `npx playwright install chromium`. Not wired into CI by default,
// full API-backed flows need a running server + backend.
export default defineConfig({
  testDir: './e2e',
  testMatch: process.env.APERIO_LIVE_E2E ? '**/exposes-live.spec.ts' : ['**/smoke.spec.ts', '**/exposes.spec.ts'],
  fullyParallel: true,
  forbidOnly: !!process.env.CI,
  retries: process.env.CI ? 2 : 0,
  reporter: 'list',
  use: {
    baseURL: 'http://localhost:4173',
    trace: 'on-first-retry',
    // Optional installed browser for environments without Playwright downloads.
    channel: process.env.APERIO_E2E_CHANNEL,
  },
  projects: [{ name: 'chromium', use: { ...devices['Desktop Chrome'] } }],
  webServer: process.env.APERIO_LIVE_E2E ? undefined : {
    command: 'npm run build && npm run preview -- --port 4173',
    url: 'http://localhost:4173',
    reuseExistingServer: !process.env.CI,
    timeout: 120_000,
  },
})
