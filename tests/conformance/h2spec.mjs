// Runs h2spec against Aperio's inbound HTTP/2, twice, and gates on the
// difference (`planned_features.md` #97).
//
// The entry framed HTTP/2 as "a backend transport rather than something a
// visitor speaks to us". That turned out to be only half true, and the better
// half is the one worth testing: `axum::serve` accepts **h2c with prior
// knowledge** from visitors, so a visitor really can speak HTTP/2 to the
// server, and h2spec, which is a conformance client for HTTP/2 *servers*, can
// be pointed straight at it. The client-side `h2://` role is not covered here
// and cannot be: h2spec tests servers, and testing our client would need a
// deliberately non-conformant server, which is a different tool.
//
// The topology, run twice against one server:
//
//   [ h2spec ] --h2c--> [ aperio-server ]                        (baseline)
//   [ h2spec ] --h2c--> [ aperio-server ] ==tunnel==> [ backend ] (proxied)
//
// **The gate is the delta, and that is the whole design.** Almost every case
// here exercises frame and connection handling that belongs to hyper rather
// than to Aperio, so an absolute count says something about the stack and not
// about this project. What says something about *this project* is a case that
// passes when the server answers for itself and fails when the same server is
// proxying: that difference is the relay, and nothing else. It is the same
// reasoning that made the Autobahn run use a backend which passes the suite
// on its own.
//
// A delta is re-confirmed before it fails the run, because measuring showed
// the GOAWAY cases to be timing-sensitive on this stack: over four runs,
// "Sends a GOAWAY frame" failed three times and "GOAWAY with unknown error
// code" once. A gate that flakes is worse than no gate, so a case that only
// fails on one side is run again on both before it is believed.
//
// Usage:
//   node tests/conformance/h2spec.mjs [--keep]
//
// Needs the h2spec binary (downloaded on demand into .h2spec/) and the debug
// binaries in target/.
import { spawn } from 'node:child_process'
import { chmod, mkdir, readFile, rm, writeFile } from 'node:fs/promises'
import { existsSync } from 'node:fs'
import { createServer } from 'node:net'
import { createServer as httpServer, request as httpRequest } from 'node:http'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'

const here = dirname(fileURLToPath(import.meta.url))
const root = join(here, '..', '..')
const reportDir = join(here, 'reports')
const toolDir = join(here, '.h2spec')

const H2SPEC_VERSION = 'v2.6.0'

const keep = process.argv.includes('--keep')
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

function get(port, path) {
  return new Promise((resolve, reject) => {
    const req = httpRequest({ hostname: '127.0.0.1', port, path }, (res) => {
      res.resume()
      res.on('end', () => resolve(res.statusCode ?? 0))
    })
    req.on('error', reject)
    req.end()
  })
}

function start(label, command, argv, env = {}) {
  const proc = spawn(command, argv, {
    env: { ...process.env, ...env },
    stdio: ['ignore', 'pipe', 'pipe'],
  })
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

/**
 * Waits for the tunnel to serve `/`, and says why when it does not.
 *
 * The wait used to fail with the timeout alone, which is the least useful half
 * of what happened: both processes are running and both have written down what
 * they think. This failure took a round trip to diagnose for exactly that
 * reason, and the answer was in the server's log the whole time ("declares no
 * gate and is not declared open").
 */
async function routable(serverPort) {
  try {
    await waitFor(async () => (await get(serverPort, '/')) === 200, 'the tunnel to become routable')
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

/** The h2spec binary, downloaded once into `.h2spec/`. */
async function h2specBinary() {
  const bin = join(toolDir, 'h2spec')
  if (existsSync(bin)) return bin
  const arch = process.arch === 'x64' ? 'amd64' : process.arch === 'arm64' ? 'amd64' : null
  const os = { darwin: 'darwin', linux: 'linux' }[process.platform]
  if (!os || !arch) {
    throw new Error(`no h2spec build for ${process.platform}/${process.arch}`)
  }
  // Only amd64 is published; on Apple Silicon it runs under Rosetta, which is
  // how this was developed.
  const asset = `h2spec_${os}_${arch}.tar.gz`
  const url = `https://github.com/summerwind/h2spec/releases/download/${H2SPEC_VERSION}/${asset}`
  await mkdir(toolDir, { recursive: true })
  const tarball = join(toolDir, asset)
  console.log(`Downloading ${url}`)
  await run('curl', ['-sSfL', '-o', tarball, url])
  await run('tar', ['xzf', tarball, '-C', toolDir])
  await chmod(bin, 0o755)
  return bin
}

function run(command, argv, options = {}) {
  return new Promise((resolve, reject) => {
    const proc = spawn(command, argv, { stdio: 'inherit', ...options })
    proc.on('error', reject)
    proc.on('exit', (code) => (code === 0 ? resolve() : reject(new Error(`${command} exited ${code}`))))
  })
}

/**
 * One h2spec run, returning every case and whether it passed.
 *
 * The JUnit report is read rather than the console output, and it has to be
 * cleaned first: h2spec copies raw frame payloads into it, so a report can
 * contain bytes that are not legal XML. Those are the opaque data of a PING,
 * not something worth failing over.
 */
async function h2spec(bin, port, path, reportPath, sections = []) {
  await new Promise((resolve) => {
    const proc = spawn(
      bin,
      ['-h', '127.0.0.1', '-p', String(port), '-P', path, '-j', reportPath, ...sections],
      { stdio: ['ignore', 'pipe', 'pipe'] },
    )
    let out = ''
    proc.stdout.on('data', (c) => (out += c.toString()))
    proc.stderr.on('data', (c) => (out += c.toString()))
    // A non-zero exit means cases failed, which is data rather than an error.
    proc.on('exit', () => resolve(out))
  })

  const raw = await readFile(reportPath, 'utf8')
  const clean = raw.replace(/[^\t\n\r\x20-퟿-�]/g, '?')
  const cases = new Map()
  // Deliberately not an XML parser dependency: two attributes and the
  // presence of a child element is the whole question being asked.
  for (const block of clean.split('<testcase ').slice(1)) {
    const pkg = /package="([^"]*)"/.exec(block)?.[1] ?? '?'
    const name = /classname="([^"]*)"/.exec(block)?.[1] ?? '?'
    const body = block.split('</testcase>')[0]
    const ok = !body.includes('<failure') && !body.includes('<error')
    cases.set(`${pkg} :: ${name}`, ok)
  }
  if (cases.size === 0) throw new Error(`h2spec produced no cases in ${reportPath}`)
  return cases
}

function summarize(cases) {
  const failed = [...cases].filter(([, ok]) => !ok).map(([k]) => k)
  return { total: cases.size, failed }
}

async function main() {
  const backendPort = await freePort()
  const serverPort = await freePort()
  const dataDir = join(here, '.data-h2')
  await rm(dataDir, { recursive: true, force: true })
  await mkdir(dataDir, { recursive: true })
  await mkdir(reportDir, { recursive: true })

  const bin = await h2specBinary()
  const token = `conformance-${Date.now()}`
  const binary = (name) =>
    join(process.env.CARGO_TARGET_DIR ?? join(root, 'target'), 'debug', name)

  // A backend that answers everything, because h2spec cares about frames and
  // not about what is behind them.
  const backend = httpServer((_req, res) => {
    res.writeHead(200, { 'content-type': 'text/plain' })
    res.end('ok')
  })
  await new Promise((r) => backend.listen(backendPort, '127.0.0.1', r))

  start('server', binary('aperio-server'), [], {
    PORT: String(serverPort),
    APERIO_SERVER_TOKEN: token,
    APERIO_DATA_DIR: dataDir,
    APERIO_DASHBOARD: '0',
  })
  await waitFor(async () => (await get(serverPort, '/aperio/health')) === 200, 'aperio-server')

  // Bound on a path rather than a hostname, for the reason the Autobahn run
  // records: h2spec's authority is whatever it dialled, so a hostname bind
  // would make this depend on what the machine is called.
  start('client', binary('aperio-client'), [], {
    APERIO_SERVER_URL: `http://127.0.0.1:${serverPort}`,
    APERIO_SERVER_TOKEN: token,
    APERIO_TARGET: `http://127.0.0.1:${backendPort}`,
    APERIO_PATH: '/',
    // Declared open, because these runs grade the *protocol*, not the visitor
    // gate. The server has been closed by default since 0.10.0, so a route
    // that declares nothing is refused before a frame is ever exchanged, and
    // the grader then reports a transport problem it did not cause.
    APERIO_PUBLIC: '1',
    APERIO_CONNECTIONS: '1',
  })
  await routable(serverPort)

  console.log('Running h2spec against the server itself (baseline)...')
  const baseline = await h2spec(bin, serverPort, '/aperio/health', join(reportDir, 'h2spec-baseline.xml'))
  console.log('Running h2spec through the tunnel...')
  const proxied = await h2spec(bin, serverPort, '/', join(reportDir, 'h2spec-proxied.xml'))

  const b = summarize(baseline)
  const p = summarize(proxied)
  console.log(`\nbaseline: ${b.total - b.failed.length}/${b.total} passed`)
  for (const f of b.failed) console.log(`  fails on both paths (the stack's): ${f}`)
  console.log(`proxied:  ${p.total - p.failed.length}/${p.total} passed`)

  // The gate: a case the server passes for itself and fails while proxying.
  let regressions = [...proxied].filter(([k, ok]) => !ok && baseline.get(k) === true).map(([k]) => k)

  if (regressions.length > 0) {
    console.log(`\n${regressions.length} case(s) differ; re-running both sides to confirm...`)
    const baseline2 = await h2spec(bin, serverPort, '/aperio/health', join(reportDir, 'h2spec-baseline-2.xml'))
    const proxied2 = await h2spec(bin, serverPort, '/', join(reportDir, 'h2spec-proxied-2.xml'))
    regressions = regressions.filter((k) => proxied2.get(k) === false && baseline2.get(k) === true)
    if (regressions.length === 0) {
      console.log('None of them held on the second run: timing, not a relay bug.')
    }
  }

  await writeFile(
    join(reportDir, 'h2spec-summary.json'),
    JSON.stringify(
      {
        h2spec: H2SPEC_VERSION,
        total: p.total,
        baselineFailures: b.failed,
        proxiedFailures: p.failed,
        confirmedRegressions: regressions,
      },
      null,
      2,
    ),
  )

  if (regressions.length > 0) {
    console.error('\nCases that pass on the server and fail through the tunnel:')
    for (const r of regressions) console.error(`  ${r}`)
    return 1
  }
  console.log('\nNo case regressed through the tunnel.')
  return 0
}

let code = 1
try {
  code = await main()
} catch (e) {
  console.error(e)
} finally {
  if (!keep) await stopAll()
  process.exit(code)
}
