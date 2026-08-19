// The scheduled soak: bring the stack up, put load on it, and watch RSS
// (`planned_features.md` #98).
//
// The README says the server "idles at ~14 MB RSS; the client is 6 MB ... .
// Neither grows with request count." That is a claim about a shape over time,
// and the only thing that keeps it true is measuring it on a schedule rather
// than when somebody already suspects a leak, which is after the fact.
//
//   node tests/soak/run.mjs                     # the full run
//   node tests/soak/run.mjs --plateau 30s       # shorter, for trying it out
//   node tests/soak/run.mjs --no-load           # plumbing only, no traffic
//   node tests/soak/run.mjs --profile debug     # against the debug binaries
//
// `--no-load` exists to check the harness itself without generating load,
// which is how this was developed: the machine it was written on is not a
// load-testing machine, so the traffic run is left to the schedule. The rule
// that decides pass or fail lives in `trend.mjs` and is unit-tested against
// series whose answer is known, so the part that can be wrong quietly is the
// part that does not need a soak to check.
import { spawn } from 'node:child_process'
import { mkdir, rm, writeFile } from 'node:fs/promises'
import { createServer } from 'node:net'
import { createServer as httpServer, request as httpRequest } from 'node:http'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { judge, mb } from './trend.mjs'

const here = dirname(fileURLToPath(import.meta.url))
const root = join(here, '..', '..')

function arg(name, fallback) {
  const i = process.argv.indexOf(`--${name}`)
  return i === -1 ? fallback : process.argv[i + 1]
}
const noLoad = process.argv.includes('--no-load')
const plateau = duration(arg('plateau', '5m'))
const ramp = duration(arg('ramp', '30s'))
const sampleEvery = duration(arg('sample-every', '5s'))
const vus = Number(arg('vus', '20'))

function duration(text) {
  const m = /^(\d+)(ms|s|m)$/.exec(String(text))
  if (!m) throw new Error(`unparseable duration: ${text}`)
  return Number(m[1]) * { ms: 1, s: 1000, m: 60_000 }[m[2]]
}

const children = []
function start(label, command, argv, env = {}) {
  const proc = spawn(command, argv, {
    env: { ...process.env, ...env },
    stdio: ['ignore', 'pipe', 'pipe'],
  })
  let output = ''
  const collect = (c) => (output += c.toString())
  proc.stdout.on('data', collect)
  proc.stderr.on('data', collect)
  const child = { label, proc, log: () => output }
  children.push(child)
  return child
}

/**
 * Waits for the tunnel to serve `/`, and says why when it does not.
 *
 * The same helper the conformance harnesses grew, for the same reason: the
 * timeout alone says the least useful half of what happened, and what broke
 * this run was written plainly in the server's log.
 */
async function routable(serverPort, host) {
  try {
    await waitFor(async () => (await get(serverPort, '/', host)) === 200, 'the tunnel')
  } catch (e) {
    for (const label of ['server', 'client']) {
      const child = children.find((c) => c.label === label)
      if (child) console.error(`--- ${label} log (tail) ---\n${child.log().slice(-3000)}`)
    }
    throw e
  }
}

async function stopAll() {
  for (const { proc } of children) {
    if (proc.exitCode === null && proc.signalCode === null) proc.kill('SIGTERM')
  }
  await new Promise((r) => setTimeout(r, 500))
  for (const { proc } of children) {
    if (proc.exitCode === null && proc.signalCode === null) proc.kill('SIGKILL')
  }
}

function freePort() {
  return new Promise((resolve, reject) => {
    const probe = createServer()
    probe.once('error', reject)
    probe.listen(0, '127.0.0.1', () => {
      const { port } = probe.address()
      probe.close(() => resolve(port))
    })
  })
}

function get(port, path, host) {
  return new Promise((resolve, reject) => {
    const req = httpRequest(
      { hostname: '127.0.0.1', port, path, headers: host ? { host } : {} },
      (res) => {
        res.resume()
        res.on('end', () => resolve(res.statusCode ?? 0))
      },
    )
    req.on('error', reject)
    req.end()
  })
}

async function waitFor(check, label, timeoutMs = 60_000) {
  const deadline = Date.now() + timeoutMs
  for (;;) {
    try {
      if (await check()) return
    } catch {
      // Not up yet.
    }
    if (Date.now() >= deadline) throw new Error(`timed out waiting for ${label}`)
    await new Promise((r) => setTimeout(r, 200))
  }
}

/**
 * Resident set size of a pid, in bytes.
 *
 * `ps` rather than anything in-process: the numbers wanted are the two Rust
 * binaries', and asking the operating system the same way an operator would
 * is both simpler and the same answer they will get when they check.
 */
function rss(pid) {
  return new Promise((resolve) => {
    const proc = spawn('ps', ['-o', 'rss=', '-p', String(pid)])
    let out = ''
    proc.stdout.on('data', (c) => (out += c.toString()))
    proc.on('error', () => resolve(null))
    proc.on('exit', () => {
      const kb = Number(out.trim())
      resolve(Number.isFinite(kb) && kb > 0 ? kb * 1024 : null)
    })
  })
}

async function main() {
  const backendPort = await freePort()
  const serverPort = await freePort()
  const dataDir = join(here, '.data')
  const reportDir = join(here, 'reports')
  await rm(dataDir, { recursive: true, force: true })
  await mkdir(dataDir, { recursive: true })
  await mkdir(reportDir, { recursive: true })

  const token = `soak-${Date.now()}`
  const host = 'soak.local'
  // Release by default: a soak measures the binary people run, and a debug
  // build's allocation behaviour is not it. `--profile debug` is for checking
  // the harness itself without waiting for a release build.
  const profile = arg('profile', 'release')
  const binary = (name) =>
    join(process.env.CARGO_TARGET_DIR ?? join(root, 'target'), profile, name)

  // A backend that does as little as possible, so what is being measured is
  // the tunnel rather than whatever is behind it.
  const backend = httpServer((_req, res) => {
    res.writeHead(200, { 'content-type': 'text/plain' })
    res.end('ok')
  })
  await new Promise((r) => backend.listen(backendPort, '127.0.0.1', r))

  const server = start('server', binary('aperio-server'), [], {
    PORT: String(serverPort),
    APERIO_SERVER_TOKEN: token,
    APERIO_DATA_DIR: dataDir,
    APERIO_DASHBOARD: '0',
    // The per-visitor limiter would otherwise be the thing under test. These
    // are the names the server actually reads: `APERIO_RATE_LIMIT` was written
    // here and is not a setting, so this run measured memory with the limiter
    // on for as long as the line has existed.
    APERIO_IP_LIMIT_MAX: '1000000',
    APERIO_IP_LIMIT_REFILL: '100000',
  })
  await waitFor(async () => (await get(serverPort, '/aperio/health')) === 200, 'aperio-server')

  const client = start('client', binary('aperio-client'), [], {
    APERIO_SERVER_URL: `http://127.0.0.1:${serverPort}`,
    APERIO_SERVER_TOKEN: token,
    APERIO_TARGET: `http://127.0.0.1:${backendPort}`,
    APERIO_HOSTNAME: host,
    // Declared open. This run measures memory under sustained load, not the
    // visitor gate, and the server has been closed by default since 0.10.0:
    // a route that declares nothing is refused, and the soak then fails as a
    // tunnel that never became routable.
    APERIO_PUBLIC: '1',
    APERIO_CONNECTIONS: '1',
  })
  await routable(serverPort, host)

  let load = null
  if (!noLoad) {
    // k6 drives the load, from the profile that was already in the tree.
    load = start('k6', 'k6', [
      'run',
      '--quiet',
      '-e',
      `BASE_URL=http://127.0.0.1:${serverPort}`,
      '-e',
      `HOST=${host}`,
      '-e',
      `VUS=${vus}`,
      '-e',
      `RAMP=${Math.round(ramp / 1000)}s`,
      '-e',
      `PLATEAU=${Math.round(plateau / 1000)}s`,
      join(here, 'k6.js'),
    ])
    load.proc.on('error', () => {
      console.error('k6 is not installed; run with --no-load to check the harness only')
    })
  }

  // The ramp is skipped on purpose: memory is *supposed* to rise while load
  // is being added, and a rule that looked at the ramp would be measuring the
  // ramp. Only the plateau is judged.
  console.log(`Ramping for ${ramp / 1000}s${noLoad ? ' (no load)' : ''}...`)
  await new Promise((r) => setTimeout(r, ramp))

  console.log(`Sampling the plateau for ${plateau / 1000}s every ${sampleEvery / 1000}s...`)
  const samples = { server: [], client: [] }
  const startedAt = Date.now()
  while (Date.now() - startedAt < plateau) {
    const atMs = Date.now() - startedAt
    const [s, c] = await Promise.all([rss(server.proc.pid), rss(client.proc.pid)])
    if (s) samples.server.push({ atMs, rssBytes: s })
    if (c) samples.client.push({ atMs, rssBytes: c })
    await new Promise((r) => setTimeout(r, sampleEvery))
  }

  if (load && load.proc.exitCode === null) load.proc.kill('SIGTERM')

  const verdicts = {
    server: judge(samples.server),
    client: judge(samples.client),
  }
  const report = {
    ranAt: new Date(startedAt).toISOString(),
    plateauMs: plateau,
    sampleEveryMs: sampleEvery,
    load: noLoad ? 'none' : `k6, ${vus} VUs`,
    verdicts,
    samples,
  }
  await writeFile(join(reportDir, 'soak.json'), JSON.stringify(report, null, 2))

  let bad = false
  for (const [who, v] of Object.entries(verdicts)) {
    console.log(
      `\n${who}: ${v.verdict}` +
        (v.verdict === 'inconclusive'
          ? ` (${v.reason})`
          : `\n  first ${mb(v.firstRssBytes)}, last ${mb(v.lastRssBytes)}` +
            `\n  slope ${mb(v.slopeBytesPerMinute)}/min, projected ${mb(v.projectedGrowthBytes)} over the plateau` +
            `\n  quarters ${mb(v.startMedianBytes)} -> ${mb(v.endMedianBytes)}, allowed ${mb(v.allowedBytes)}`),
    )
    // Inconclusive fails too: a run that could not measure is not evidence
    // that nothing grew, and reporting it as a pass is how a broken schedule
    // goes unnoticed for months.
    if (v.verdict !== 'flat') bad = true
  }

  console.log(`\nReport: ${join(reportDir, 'soak.json')}`)
  return bad ? 1 : 0
}

let code = 1
try {
  code = await main()
} catch (e) {
  console.error(e)
} finally {
  await stopAll()
  process.exit(code)
}
