import { expect, test } from '@playwright/test'

// Shell smoke test: the built app is served and its React root mounts. Runs
// against a static preview (no backend), so it asserts the shell boots rather
// than any API-backed flow. Extend with API-backed journeys against a running
// Aperio server in a fuller e2e environment.
test('dashboard shell boots', async ({ page }) => {
  const response = await page.goto('/')
  expect(response?.ok()).toBeTruthy()
  // The Vite mount point exists and the bundle attached to it.
  await expect(page.locator('#root')).toBeAttached()
})
