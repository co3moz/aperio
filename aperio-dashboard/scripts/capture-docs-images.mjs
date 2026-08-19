#!/usr/bin/env node
/**
 * Re-captures the dashboard screenshots embedded in `README.md` and
 * `docs/dashboard.md`.
 *
 * The images were taken by hand once and then went stale: the version in the
 * corner, the sidebar, the whole visual language moved on while the files sat
 * there. So this is a script rather than a note about how to do it.
 *
 * It brings up a throwaway instance, server on a temp data directory, a demo
 * backend, one client that declares a service and a tunnel, drives some
 * traffic through it so the screens have something real to show, and captures
 * each page with Playwright at 2x. Nothing outside the temp directory is
 * touched, and everything is stopped afterwards, whether it succeeded or not.
 *
 *   node scripts/capture-docs-images.mjs            # build first if needed
 *   node scripts/capture-docs-images.mjs --keep     # leave the instance up
 *
 * Requires the debug binaries (`cargo build --workspace`) and Playwright's
 * chromium (`npx playwright install chromium`).
 */
import { spawn } from 'node:child_process'
import { request as httpRequest } from 'node:http'
import { chromium } from '@playwright/test'
import { mkdtempSync, rmSync, writeFileSync, mkdirSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const HERE = fileURLToPath(new URL('.', import.meta.url))
const REPO = resolve(HERE, '../..')
const IMAGES = join(REPO, 'docs/images')
const KEEP = process.argv.includes('--keep')

// Ports well away from the defaults, so a capture cannot collide with the
// instance someone is already running on this machine.
const PORT = 8781
const BACKEND_PORT = 8782
const BASE = `http://127.0.0.1:${PORT}`
const TOKEN = 'apr_docs_capture_master_token'

// 1440x900 at 2x: the size the existing images were taken at, so a refresh
// drops into the docs without every figure changing scale.
const VIEWPORT = { width: 1440, height: 900 }
const SCALE = 2

const children = []
/** The run's temp directory, removed in the `finally`, including after a
 *  failure, which is when the leftovers used to pile up. */
let workDir = null
const sleep = (ms) => new Promise((r) => setTimeout(r, ms))

function start(name, cmd, args, opts = {}) {
  const child = spawn(cmd, args, { stdio: 'ignore', ...opts })
  child.on('error', (e) => console.error(`${name}: ${e.message}`))
  children.push(child)
  return child
}

/** Stops everything and waits for it, so the temp directory can be removed:
 *  a server still flushing its data directory makes that an ENOTEMPTY. */
async function stopAll() {
  await Promise.all(
    children.map(
      (child) =>
        new Promise((resolve) => {
          if (child.exitCode !== null || child.signalCode) return resolve()
          child.once('exit', resolve)
          try {
            child.kill('SIGTERM')
          } catch {
            resolve()
          }
          setTimeout(resolve, 3000)
        }),
    ),
  )
}

/** Waits for `check` to pass, or gives up with a message naming what failed. */
async function waitFor(what, check, attempts = 60) {
  for (let i = 0; i < attempts; i++) {
    try {
      if (await check()) return
    } catch {
      // Not up yet.
    }
    await sleep(500)
  }
  throw new Error(`${what} did not come up`)
}

const api = (path, init) => fetch(`${BASE}${path}`, init)

async function main() {
  const dir = mkdtempSync(join(tmpdir(), 'aperio-docs-capture-'))
  workDir = dir
  const dataDir = join(dir, 'data')
  const clientDir = join(dir, 'client')
  mkdirSync(dataDir, { recursive: true })
  mkdirSync(clientDir, { recursive: true })

  // --- the demo backend: enough shape that the inspector has a real body ---
  const backend = join(dir, 'backend.mjs')
  writeFileSync(
    backend,
    `import { createServer } from 'node:http'
createServer((req, res) => {
  if (req.url.startsWith('/missing')) {
    res.writeHead(404, { 'content-type': 'application/json' })
    res.end(JSON.stringify({ error: 'not found' }))
    return
  }
  let body = ''
  req.on('data', (c) => (body += c))
  req.on('end', () => {
    res.writeHead(200, { 'content-type': 'application/json', 'cache-control': 'max-age=60' })
    res.end(
      JSON.stringify(
        {
          service: 'checkout-api',
          method: req.method,
          path: req.url,
          received: body ? JSON.parse(body) : null,
          items: [
            { id: 'itm_8241', name: 'Annual plan', amount: 4900, currency: 'EUR' },
            { id: 'itm_8242', name: 'Seat add-on', amount: 1200, currency: 'EUR' },
          ],
        },
        null,
        2,
      ),
    )
  })
}).listen(${BACKEND_PORT}, '127.0.0.1')
`,
  )
  start('backend', process.execPath, [backend])

  // --- the server, on its own data directory ---
  start('server', join(REPO, 'target/debug/aperio-server'), [], {
    cwd: dir,
    env: {
      ...process.env,
      APERIO_DATA_DIR: dataDir,
      APERIO_SERVER_TOKEN: TOKEN,
      // The server reads the bare `HOST`/`PORT`, not `APERIO_`-prefixed ones.
      PORT: String(PORT),
      HOST: '127.0.0.1',
      APERIO_METRICS: '1',
      // Every request in a capture comes from 127.0.0.1, so the per-visitor
      // rate limit sees one very busy visitor and fills the traffic table with
      // its own refusals. Lifted here because it is an artifact of capturing,
      // not something a deployment with real visitors would hit.
      APERIO_IP_LIMIT_MAX: '100000',
      APERIO_IP_LIMIT_REFILL: '10000',
      // The autoscaling screen is a list of armed records, so the feature has
      // to be on for the client's declaration to be honored at all.
      APERIO_SCALING: '1',
    },
  })
  await waitFor('the server', async () => (await api('/aperio/auth')).ok)

  // --- one client: a named service and a declared tunnel ---
  writeFileSync(
    join(clientDir, 'aperio.yaml'),
    `server:
  url: ${BASE}
  token: ${TOKEN}
client_id: 7c1f6b6e-2a5d-4f2a-9c3e-0b6a1d5e4f21
services:
  - name: checkout_api
    custom_name: "Checkout API"
    target: http://127.0.0.1:${BACKEND_PORT}
    hostname: api.example.com
    # Declared open, as a public API would be. Without it the server refuses
    # the route (closed by default since 0.10.0), the demo traffic never
    # arrives, and every screen is captured as an empty state, which looks
    # exactly like a successful capture until somebody opens the PDF.
    public: true
scaling:
  url: https://api.provider.example/apps/checkout/scale
  min: 0
  max: 8
  cold_start: 45s
tunnels:
  - name: pg_main
    custom_name: "Primary Postgres"
    target: 127.0.0.1:5432
`,
  )
  start('client', join(REPO, 'target/debug/aperio-client'), [], { cwd: clientDir })
  // The dashboard API answers to a session, not to the master token: without
  // one it redirects to the login page, which is a perfectly successful
  // response and would have this wait pass for the wrong reason.
  const session = await login()
  await waitFor('the tunnel client', async () => {
    const res = await api('/aperio/api/stats', { headers: { cookie: session } })
    if (!res.ok) return false
    const stats = await res.json()
    return stats.connected_clients_count > 0
  })

  // A screen with an empty state says nothing about the product, so the few
  // rows these pages are *for* are created here rather than left to chance.
  await seed(session)

  await driveTraffic()
  // Said out loud: a capture of an idle-looking dashboard is the failure mode
  // here, and it looks exactly like a successful run otherwise.
  const snapshot = await (await api('/aperio/api/stats', { headers: { cookie: session } })).json()
  console.log(
    `traffic: ${snapshot.total_requests} requests, ` +
      `${snapshot.failed_requests} failed, ${snapshot.connected_clients_count} client(s)`,
  )
  if (snapshot.total_requests < 100) {
    throw new Error('the demo traffic did not reach the server being captured')
  }

  // --- capture ---
  const browser = await chromium.launch()
  const context = await browser.newContext({
    viewport: VIEWPORT,
    deviceScaleFactor: SCALE,
    colorScheme: 'dark',
    // The docs show the English UI. Without an explicit choice the app reads
    // the browser's languages, so setting the locale is the whole of it, no
    // click, no reload, and no seeded storage to go stale when the key moves.
    locale: 'en-US',
  })
  const page = await context.newPage()

  await page.goto(`${BASE}/aperio/auth`)
  await page.evaluate(
    async ([base, token]) => {
      await fetch(`${base}/aperio/auth`, {
        method: 'POST',
        headers: { Authorization: `Basic ${btoa(`aperio:${token}`)}` },
      })
    },
    [BASE, TOKEN],
  )

  for (const shot of SHOTS) {
    // `/aperio`, with no trailing slash: `/aperio/` is not the dashboard, it
    // falls through to the proxy and reaches a tunnel client instead.
    await page.goto(`${BASE}/aperio?tab=${shot.tab}`)
    // Not `networkidle`: the dashboard holds an event stream open, so the
    // network is never idle and the wait only ever expires. The shell first,
    // then whatever this particular screen is made of.
    await page.getByText('ORGANIZATION').first().waitFor({ timeout: 30_000 })
    if (shot.ready) await page.locator(shot.ready).first().waitFor({ timeout: 30_000 })
    if (shot.prepare) await shot.prepare(page)
    // The request-rate chart animates in; let it settle so two captures of the
    // same screen do not differ by a sweep of the line.
    await sleep(1200)
    const file = join(IMAGES, `${shot.name}.png`)
    await page.screenshot({ path: file })
    console.log(`wrote docs/images/${shot.name}.png`)
  }

  await browser.close()
}

/** Creates the handful of rows the token, maintenance and share screens show.
 *
 *  Each is the ordinary API call the dashboard itself makes, so a capture
 *  cannot drift from what a real operator would end up looking at. */
async function seed(session) {
  // A seed that fails quietly leaves a screen showing its empty state, which
  // is indistinguishable from a successful capture until someone opens the
  // PDF. So a refusal stops the run and says which call was refused.
  const post = async (path, body) => {
    const res = await api(path, {
      method: 'POST',
      headers: { cookie: session, 'content-type': 'application/json' },
      body: JSON.stringify(body),
    })
    if (!res.ok) {
      throw new Error(`seeding ${path} failed: HTTP ${res.status} ${await res.text()}`)
    }
  }

  await post('/aperio/api/tokens', {
    name: 'ci_previews',
    hostnames: ['preview.example.com'],
    paths: [],
    max_rps: 50,
    ttl_seconds: 30 * 24 * 3600,
  })
  await post('/aperio/api/tokens', {
    name: 'checkout_api',
    hostnames: ['api.example.com'],
    paths: [],
  })
  await post('/aperio/api/maintenance', {
    hostname: 'legacy.example.com',
    enabled: true,
    reason: 'database migration',
    ttl_seconds: 3600,
  })
  await post('/aperio/api/share', {
    hostname: 'api.example.com',
    path: '/docs',
    ttl_seconds: 86400,
  })
}

/** Signs in as the built-in master admin and returns the session cookie. */
async function login() {
  const res = await api('/aperio/auth', {
    method: 'POST',
    headers: { Authorization: `Basic ${Buffer.from(`aperio:${TOKEN}`).toString('base64')}` },
    redirect: 'manual',
  })
  const cookie = res.headers.get('set-cookie')
  if (!cookie) throw new Error(`login did not set a session cookie (HTTP ${res.status})`)
  return cookie.split(';')[0]
}

/**
 * One request through the proxy, as a visitor of `api.example.com` makes it.
 *
 * `node:http` rather than `fetch`: the visitor's identity here *is* the `Host`
 * header, and fetch drops it, a forbidden header name. Every request went to
 * the dashboard's own host instead, the traffic screens stayed empty, and the
 * capture looked like a working run of an idle system.
 */
function visit(path, { method = 'GET', body } = {}) {
  return new Promise((resolve, reject) => {
    const req = httpRequest(
      {
        host: '127.0.0.1',
        port: PORT,
        path,
        method,
        headers: {
          host: 'api.example.com',
          ...(body ? { 'content-type': 'application/json' } : {}),
        },
      },
      (res) => {
        res.resume()
        res.on('end', () => resolve(res.statusCode))
      },
    )
    req.on('error', reject)
    if (body) req.write(body)
    req.end()
  })
}

/**
 * Traffic worth showing: enough requests for the tiles to read as a live
 * system, a handful of failures so the success ratio is not a flat 100%, and
 * one POST with a body for the inspector to open.
 */
async function driveTraffic() {
  for (let i = 0; i < 120; i++) {
    await visit(`/v1/items?page=${i}`)
  }
  for (let i = 0; i < 4; i++) {
    await visit('/missing')
  }
  await visit('/v1/checkout', {
    method: 'POST',
    body: JSON.stringify({ cart: ['itm_8241', 'itm_8242'], currency: 'EUR' }),
  })
  // A short gap so the live chart shows a slope rather than one spike at the
  // right edge.
  await sleep(1500)
  for (let i = 0; i < 30; i++) {
    await visit(`/v1/items?page=${i}`)
  }
}

/** The figures the docs and the guide embed, in the order they appear. */
const SHOTS = [
  // `ready` is a selector that only exists once the screen has its data, so a
  // capture is never of a skeleton or an empty state that is about to fill.
  {
    name: 'dashboard-overview',
    tab: 'overview',
    ready: 'svg',
    // The request-rate chart is drawn from samples the *open page* receives,
    // so traffic that ran before the browser existed leaves it flat. This is
    // the hero image in the README: give it something to draw.
    prepare: async () => {
      const until = Date.now() + 24_000
      let n = 0
      while (Date.now() < until) {
        const burst = 3 + Math.floor((Date.now() / 1000) % 7)
        await Promise.all(
          Array.from({ length: burst }, () =>
            // One in twenty is a miss, so the status mix on the traffic screen
            // is a real mix rather than a solid green bar.
            visit(n++ % 20 === 0 ? '/missing' : `/v1/items?page=${n}`),
          ),
        )
        await sleep(400)
      }
    },
  },
  { name: 'dashboard-clients', tab: 'clients', ready: 'table tbody tr' },
  { name: 'dashboard-traffic', tab: 'traffic', ready: 'table tbody tr' },
  { name: 'dashboard-topology', tab: 'topology', ready: 'svg' },
  {
    name: 'dashboard-inspector',
    tab: 'traffic',
    ready: 'table tbody tr',
    // The inspector is a dialog over the traffic table: open the newest row.
    prepare: async (page) => {
      await page.locator('table tbody tr').first().click()
      await page.locator('[role="dialog"]').first().waitFor({ timeout: 15_000 })
    },
  },
  { name: 'dashboard-breakdown', tab: 'breakdown', ready: 'text=api.example.com' },
  { name: 'dashboard-tunnels', tab: 'tunnels', ready: 'text=pg_main' },
  { name: 'dashboard-tokens', tab: 'tokens', ready: 'text=ci_previews' },
  { name: 'dashboard-maintenance', tab: 'maintenance', ready: 'text=database migration' },
  { name: 'dashboard-scaling', tab: 'scaling', ready: 'text=api.example.com' },
  {
    // The one screen that has to be driven rather than merely visited: the
    // report does not exist until somebody asks for a hostname and a path.
    name: 'dashboard-explain',
    tab: 'topology',
    ready: 'svg',
    prepare: async (page) => {
      const box = page.getByPlaceholder(/example\.com/).first()
      await box.waitFor({ timeout: 15_000 })
      await box.fill('api.example.com/v1/items')
      await box.press('Enter')
      await page.getByText('Routing').first().waitFor({ timeout: 15_000 })
      await box.scrollIntoViewIfNeeded()
    },
  },
  {
    // A dialog over whatever page was underneath, which is the point of it.
    name: 'dashboard-settings',
    tab: 'settings',
    ready: '[role="dialog"]',
  },
]

try {
  await main()
} catch (e) {
  console.error(`capture failed: ${e.message}`)
  process.exitCode = 1
} finally {
  if (KEEP) {
    console.log(`--keep: the instance is still running on ${BASE} (${workDir})`)
  } else {
    await stopAll()
    if (workDir) rmSync(workDir, { recursive: true, force: true })
  }
}
