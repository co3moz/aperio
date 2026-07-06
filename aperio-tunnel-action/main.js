// Aperio tunnel provisioning action (dependency-free node20).
//
// 1. POST /aperio/api/tunnels on the Aperio server → ephemeral token + hostname
// 2. docker run the aperio-client with that token for the rest of the job
// 3. post.js tears the container down and revokes the token
'use strict'

const { appendFileSync } = require('node:fs')
const { execFileSync } = require('node:child_process')

/** Reads an action input (empty string counts as absent). */
function input(name, fallback) {
  const value = process.env[`INPUT_${name.toUpperCase()}`]
  return value === undefined || value === '' ? fallback : value
}

function setOutput(name, value) {
  appendFileSync(process.env.GITHUB_OUTPUT, `${name}=${value}\n`)
}

/** Saves state readable by post.js via STATE_<name>. */
function saveState(name, value) {
  appendFileSync(process.env.GITHUB_STATE, `${name}=${value}\n`)
}

function fail(message) {
  console.error(`::error::${message}`)
  process.exit(1)
}

function docker(args, opts = {}) {
  return execFileSync('docker', args, { encoding: 'utf8', ...opts })
}

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms))

async function main() {
  const serverUrl = input('server-url', '').replace(/\/+$/, '')
  const serverToken = input('server-token', '')
  if (!serverUrl) fail('server-url is required')
  if (!serverToken) fail('server-token is required')

  const port = input('port')
  const target = input('target', port ? `http://127.0.0.1:${port}` : '')
  if (!target) fail('either port or target is required')

  const defaultName = `${process.env.GITHUB_REPOSITORY ?? 'tunnel'}-run-${process.env.GITHUB_RUN_ID ?? '0'}`
  const name = input('name', defaultName).slice(0, 64)

  // --- 1. Provision an ephemeral tunnel ---------------------------------
  const payload = { name, ttl_seconds: Number(input('ttl-seconds', '3600')) }
  const hostnameInput = input('hostname')
  if (hostnameInput) payload.hostname = hostnameInput

  console.log(`Provisioning tunnel "${name}" on ${serverUrl}...`)
  const res = await fetch(`${serverUrl}/aperio/api/tunnels`, {
    method: 'POST',
    headers: {
      Authorization: `Bearer ${serverToken}`,
      'Content-Type': 'application/json',
    },
    body: JSON.stringify(payload),
  })
  if (!res.ok) {
    fail(`Tunnel provisioning failed: HTTP ${res.status} — ${await res.text()}`)
  }
  const tunnel = await res.json()
  console.log(`::add-mask::${tunnel.token}`)
  console.log(`Tunnel provisioned: ${tunnel.hostname} (id ${tunnel.id})`)

  saveState('tunnelId', tunnel.id)
  setOutput('url', tunnel.url)
  setOutput('hostname', tunnel.hostname)
  setOutput('tunnel-id', tunnel.id)

  // --- 2. Run the tunnel client ------------------------------------------
  const image = input('client-image', 'ghcr.io/co3moz/aperio-client:latest')
  const container = `aperio-tunnel-${tunnel.id.slice(0, 8)}`
  saveState('container', container)

  console.log(`Starting ${image} as ${container}...`)
  docker(['pull', '--quiet', image], { stdio: 'inherit' })
  docker([
    'run', '--detach',
    '--name', container,
    '--network', 'host',
    '--env', `APERIO_SERVER_URL=${serverUrl}`,
    '--env', `APERIO_SERVER_TOKEN=${tunnel.token}`,
    '--env', `APERIO_CLIENT_TARGET=${target}`,
    image,
  ])

  // --- 3. Wait until the client reports a successful connection ----------
  const timeoutSecs = Number(input('wait-timeout', '60'))
  const deadline = Date.now() + timeoutSecs * 1000
  let connected = false
  while (Date.now() < deadline) {
    const logs = docker(['logs', container])
    if (logs.includes('Successfully connected to Aperio Server')) {
      connected = true
      break
    }
    const state = docker(['inspect', '--format', '{{.State.Running}}', container]).trim()
    if (state !== 'true') break
    await sleep(1000)
  }
  if (!connected) {
    console.error('Tunnel client did not connect in time; container logs:')
    try {
      docker(['logs', container], { stdio: 'inherit' })
    } catch {}
    fail(`Tunnel client failed to connect within ${timeoutSecs}s`)
  }

  console.log(`Tunnel is live: ${tunnel.url} → ${target}`)
  if (process.env.GITHUB_STEP_SUMMARY) {
    appendFileSync(
      process.env.GITHUB_STEP_SUMMARY,
      `### 🌐 Aperio tunnel\n\n| | |\n|---|---|\n| URL | ${tunnel.url} |\n| Target | \`${target}\` |\n| Token | \`${name}\` (revoked when the job ends) |\n`,
    )
  }
}

main().catch((e) => fail(e.stack ?? String(e)))
