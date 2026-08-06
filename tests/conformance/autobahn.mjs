// Runs the Autobahn WebSocket test suite against a *relayed* endpoint.
//
// Why this exists: the WS relay re-frames traffic through the tunnel, and
// until now its correctness was asserted by tests we wrote against our own
// understanding of the protocol. Autobahn is the external answer, several
// hundred cases covering fragmentation, close codes, UTF-8 validity in text
// frames, ping/pong and oversized payloads, written by people who were not
// looking at our code.
//
// The topology:
//
//   [ autobahn fuzzingclient ] --ws--> [ aperio-server ] ==tunnel==> [ aperio-client ] --ws--> [ ws echo ]
//
// So every frame crosses the relay twice. The backend is `ws`, which passes
// the suite on its own, which is what makes a failure here a statement about
// the relay.
//
// Usage:
//   node tests/conformance/autobahn.mjs [--keep] [--cases 1.*,7.*]
//
// Needs Docker (the suite is Python 2 and is only sanely available as the
// `crossbario/autobahn-testsuite` image) and the debug binaries in target/.
import { spawn } from 'node:child_process'
import { mkdir, readFile, rm, writeFile } from 'node:fs/promises'
import { createServer } from 'node:net'
import { request as httpRequest } from 'node:http'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'

const here = dirname(fileURLToPath(import.meta.url))
const root = join(here, '..', '..')
const reportDir = join(here, 'reports')

const IMAGE = 'crossbario/autobahn-testsuite:0.8.2'

// 12.x and 13.x are the compression cases. They need `permessage-deflate`,
// which the tunnel deliberately does not negotiate (planned_features #73,
// withdrawn), so running them would report "the extension we do not
// implement is not implemented". Excluded by name rather than tolerated in
// the results, so the report says what was run.
const EXCLUDED = ['12.*', '13.*']

const args = process.argv.slice(2)
const keep = args.includes('--keep')
const casesArg = args.indexOf('--cases')
const cases = casesArg === -1 ? ['*'] : args[casesArg + 1].split(',')

const children = []

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

/** One GET, on `node:http`.
 *
 * Not `fetch`: `Host` is on its forbidden-header list, so it is accepted,
 * ignored, and the request arrives with the socket's own authority. The
 * routability check below is entirely about the `Host` header, so on `fetch`
 * it would be asking about the wrong service and quietly getting a 404.
 */
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

function start(label, command, argv, env = {}) {
  const proc = spawn(command, argv, { env: { ...process.env, ...env }, stdio: ['ignore', 'pipe', 'pipe'] })
  let output = ''
  const collect = (c) => {
    output += c.toString()
  }
  proc.stdout.on('data', collect)
  proc.stderr.on('data', collect)
  const child = { label, proc, log: () => output }
  children.push(child)
  return child
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

async function stopAll() {
  for (const { proc } of children) {
    if (proc.exitCode === null && proc.signalCode === null) proc.kill('SIGTERM')
  }
  await new Promise((r) => setTimeout(r, 500))
  for (const { proc } of children) {
    if (proc.exitCode === null && proc.signalCode === null) proc.kill('SIGKILL')
  }
}

/**
 * How Docker reaches a listener on this host, which is not the same answer
 * everywhere: a Linux container can share the host's network namespace, and
 * Docker Desktop cannot, so it publishes a name for the host instead.
 *
 * The returned host also ends up as the authority of the URL Autobahn dials,
 * and therefore as the `Host` header the server sees. That is why the tunnel
 * is bound on a *path* rather than a hostname: nothing then has to agree
 * about what this machine is called.
 */
function dockerHostAccess() {
  return process.platform === 'linux'
    ? { networkArgs: ['--network', 'host'], host: '127.0.0.1' }
    : { networkArgs: [], host: 'host.docker.internal' }
}

async function main() {
  const backendPort = await freePort()
  const serverPort = await freePort()
  const dataDir = join(here, '.data')
  await rm(dataDir, { recursive: true, force: true })
  await mkdir(dataDir, { recursive: true })
  await rm(reportDir, { recursive: true, force: true })
  await mkdir(reportDir, { recursive: true })

  const token = `conformance-${Date.now()}`
  const bin = (name) => join(process.env.CARGO_TARGET_DIR ?? join(root, 'target'), 'debug', name)

  start('backend', process.execPath, [join(here, 'echo-backend.mjs'), String(backendPort)])
  await waitFor(async () => (await get(backendPort, '/')) === 200, 'the echo backend')

  start('server', bin('aperio-server'), [], {
    PORT: String(serverPort),
    APERIO_SERVER_TOKEN: token,
    APERIO_DATA_DIR: dataDir,
    APERIO_DASHBOARD: '0',
  })
  await waitFor(async () => (await get(serverPort, '/aperio/health')) === 200, 'aperio-server')

  start('client', bin('aperio-client'), [], {
    APERIO_SERVER_URL: `http://127.0.0.1:${serverPort}`,
    APERIO_SERVER_TOKEN: token,
    APERIO_TARGET: `http://127.0.0.1:${backendPort}`,
    // A *path* bind, not a hostname one, and that is the whole reason this
    // runs at all. Autobahn dials a URL and the authority of that URL is the
    // `Host` header the server routes on, so a hostname bind would mean
    // teaching the container to resolve a name that exists nowhere. Bound on
    // `/`, the address Autobahn happens to dial stops mattering.
    APERIO_PATH: '/',
    APERIO_CONNECTIONS: '1',
  })
  await waitFor(async () => (await get(serverPort, '/')) === 200, 'the tunnel to become routable')

  const { networkArgs, host } = dockerHostAccess()
  const config = {
    outdir: '/reports',
    servers: [
      {
        // The agent name is what the report is keyed by.
        agent: 'aperio-relay',
        url: `ws://${host}:${serverPort}`,
        options: { version: 18 },
      },
    ],
    cases,
    'exclude-cases': EXCLUDED,
    'exclude-agent-cases': {},
  }
  await writeFile(join(here, 'fuzzingclient.json'), JSON.stringify(config, null, 2))

  const dockerArgs = [
    'run',
    '--rm',
    ...networkArgs,
    '-v',
    `${here}/fuzzingclient.json:/config/fuzzingclient.json:ro`,
    '-v',
    `${reportDir}:/reports`,
    IMAGE,
    'wstest',
    '-m',
    'fuzzingclient',
    '-s',
    '/config/fuzzingclient.json',
  ]
  process.stdout.write(`docker ${dockerArgs.join(' ')}\n`)
  const code = await new Promise((resolve) => {
    const proc = spawn('docker', dockerArgs, { stdio: 'inherit' })
    proc.on('exit', (c) => resolve(c ?? 1))
  })
  if (code !== 0) throw new Error(`wstest exited ${code}`)

  return summarize()
}

/**
 * Reads the report and decides what counts as a failure.
 *
 * Autobahn grades a case as OK, NON-STRICT, INFORMATIONAL, UNIMPLEMENTED or
 * FAILED, and only the last is a bug. NON-STRICT means the implementation
 * chose a permitted-but-less-strict behaviour, which for a *relay* is often
 * the honest answer, so it is reported and not failed on. Both grades matter:
 * `behavior` covers the frames and `behaviorClose` the close handshake, and a
 * relay that gets the data right and the close code wrong is exactly the bug
 * this run exists to catch.
 */
async function summarize() {
  const index = JSON.parse(await readFile(join(reportDir, 'index.json'), 'utf8'))
  const agent = Object.keys(index)[0]
  const results = index[agent]
  const failed = []
  const nonStrict = []
  for (const [id, result] of Object.entries(results)) {
    const grades = [result.behavior, result.behaviorClose]
    if (grades.some((g) => g === 'FAILED' || g === 'WRONG CODE' || g === 'UNCLEAN')) {
      failed.push(`${id}: behavior=${result.behavior} close=${result.behaviorClose}`)
    } else if (grades.includes('NON-STRICT')) {
      nonStrict.push(id)
    }
  }
  const total = Object.keys(results).length
  process.stdout.write(`\nAutobahn: ${total} cases, ${failed.length} failed, ${nonStrict.length} non-strict\n`)
  if (nonStrict.length) process.stdout.write(`non-strict: ${nonStrict.join(', ')}\n`)
  if (failed.length) {
    process.stdout.write(`\nFailures:\n${failed.map((f) => `  ${f}`).join('\n')}\n`)
    return 1
  }
  return 0
}

let exitCode = 1
try {
  exitCode = await main()
} catch (e) {
  process.stderr.write(`${e instanceof Error ? e.stack : String(e)}\n`)
  for (const child of children) {
    process.stderr.write(`\n--- ${child.label} ---\n${child.log().slice(-4000)}\n`)
  }
} finally {
  await stopAll()
  if (!keep) await rm(join(here, '.data'), { recursive: true, force: true })
}
process.exit(exitCode)
