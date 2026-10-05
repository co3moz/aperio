import { test, expect, type BrowserContext, type APIRequestContext } from '@playwright/test'
import { spawn, type ChildProcess } from 'node:child_process'
import { createServer, connect, type Socket } from 'node:net'
import { createSocket } from 'node:dgram'
import { mkdir, writeFile } from 'node:fs/promises'
import { createWriteStream, existsSync } from 'node:fs'
import { resolve, join } from 'node:path'
import { randomUUID } from 'node:crypto'
import { once } from 'node:events'

// Opt-in real-binary release gate. Build the server/client first, then run
// APERIO_LIVE_E2E=1 npm run test:e2e. No API routes are intercepted here.
const workspace = resolve(import.meta.dirname, '../..')
const binary = (name: string) => process.env[name === 'aperio-server' ? 'APERIO_SERVER_BIN' : 'APERIO_CLIENT_BIN']
  ?? join(process.env.CARGO_TARGET_DIR ?? join(workspace, 'target'), 'debug', name + (process.platform === 'win32' ? '.exe' : ''))

async function freePort() {
  const socket = createServer(); socket.listen(0, '127.0.0.1'); await once(socket, 'listening')
  const port = (socket.address() as { port: number }).port
  await new Promise<void>((done) => socket.close(() => done())); return port
}
async function stop(proc: ChildProcess | undefined) {
  if (!proc || proc.exitCode !== null || proc.signalCode !== null) return
  const exited = once(proc, 'exit'); proc.kill('SIGTERM')
  const timer = setTimeout(() => proc.kill('SIGKILL'), 5000)
  try { await exited } finally { clearTimeout(timer) }
}
async function json(api: APIRequestContext, url: string, method = 'GET', body?: unknown) {
  const response = await api.fetch(url, { method, data: body, maxRedirects: 0 })
  expect(response.ok(), `${method} ${url}: ${response.status()} ${await response.text()}`).toBeTruthy()
  const text = await response.text()
  return text ? JSON.parse(text) : null
}
async function login(context: BrowserContext, url: string, username: string, password: string) {
  const response = await context.request.post(`${url}/aperio/auth`, {
    headers: { authorization: `Basic ${Buffer.from(`${username}:${password}`).toString('base64')}` }, maxRedirects: 0,
  })
  expect(response.status()).toBe(200)
  expect((await context.cookies(url)).some((cookie) => cookie.name === 'aperio_session')).toBeTruthy()
}
async function tcpEcho(port: number, payload: string) {
  return new Promise<string>((done, fail) => {
    const socket = connect(port, '127.0.0.1'); let received = Buffer.alloc(0)
    const timer = setTimeout(() => { socket.destroy(); fail(new Error('TCP echo deadline')) }, 5000)
    socket.on('error', (e) => { clearTimeout(timer); fail(e) })
    socket.on('connect', () => socket.write(payload))
    socket.on('data', (chunk) => {
      received = Buffer.concat([received, chunk])
      if (received.length >= Buffer.byteLength(payload)) { clearTimeout(timer); socket.destroy(); done(received.toString()) }
    })
  })
}
async function udpEcho(port: number, payload: string) {
  const socket = createSocket('udp4')
  try {
    const received = new Promise<string>((done, fail) => {
      const timer = setTimeout(() => fail(new Error('UDP echo deadline')), 5000)
      socket.once('message', (bytes) => { clearTimeout(timer); done(bytes.toString()) })
      socket.once('error', (e) => { clearTimeout(timer); fail(e) })
    })
    socket.send(Buffer.from(payload), port, '127.0.0.1')
    return await received
  } finally { socket.close() }
}

test('live delegated TCP/UDP UI journey, isolation, persistence and revocation', async ({ browser }, testInfo) => {
  test.setTimeout(180000)
  for (const name of ['aperio-server', 'aperio-client']) expect(existsSync(binary(name)), `build ${name} first`).toBeTruthy()
  const dir = testInfo.outputPath('runtime'); await mkdir(dir, { recursive: true })
  const token = `e2e-${randomUUID()}`; const password = `e2e-${randomUUID()}`
  const serverPort = await freePort(); const publicPort = await freePort(); const url = `http://127.0.0.1:${serverPort}`
  const sockets = new Set<Socket>()
  const backend = createServer((socket) => { sockets.add(socket); socket.on('close', () => sockets.delete(socket)); socket.on('data', (bytes) => socket.write(bytes)) })
  backend.listen(0, '127.0.0.1'); await once(backend, 'listening')
  const backendPort = (backend.address() as { port: number }).port
  const datagrams = createSocket('udp4'); datagrams.on('message', (bytes, peer) => datagrams.send(bytes, peer.port, peer.address))
  datagrams.bind(backendPort, '127.0.0.1'); await once(datagrams, 'listening')
  const serverConfig = join(dir, 'server.yaml'); await writeFile(serverConfig, '{}\n')
  const processes: ChildProcess[] = []; let server: ChildProcess | undefined
  const run = (name: string, args: string[], env: Record<string, string>) => {
    const log = createWriteStream(join(dir, `${name}.log`), { flags: 'a' })
    const proc = spawn(binary(name), args, { cwd: dir, windowsHide: true, env: { ...process.env, RUST_LOG: 'info', ...env }, stdio: ['ignore', 'pipe', 'pipe'] })
    proc.stdout?.pipe(log, { end: false }); proc.stderr?.pipe(log, { end: false }); proc.once('exit', () => log.end())
    processes.push(proc); return proc
  }
  const startServer = () => server = run('aperio-server', [], { PORT: String(serverPort), APERIO_HOST: '127.0.0.1',
    APERIO_SERVER_TOKEN: token, APERIO_SERVER_CONFIG: serverConfig, APERIO_DATA_DIR: join(dir, 'data'), APERIO_DASHBOARD: 'true' })
  const admin = await browser.newContext(); const publisher = await browser.newContext(); const plain = await browser.newContext()
  const waitServer = async () => expect.poll(async () => {
    try { return (await admin.request.get(`${url}/aperio/health`, { timeout: 500 })).status() } catch { return 0 }
  }, { timeout: 20000 }).toBe(200)
  try {
    startServer(); await waitServer(); await login(admin, url, 'aperio', token)
    const org = await json(admin.request, `${url}/aperio/api/orgs`, 'POST', { name: 'acme', hostnames: [] })
    const other = await json(admin.request, `${url}/aperio/api/orgs`, 'POST', { name: 'other', hostnames: [] })
    const policy = { org_id: org.id, revision: 0,
      allocations: ['tcp', 'udp'].map((protocol) => ({ address: '127.0.0.1', protocol, first_port: publicPort, last_port: publicPort })),
      reserved: [], max_rules: 4, max_tcp_connections: 512, max_udp_sessions: 2048,
      ingress_bytes_per_second: 134217728, egress_bytes_per_second: 134217728 }
    await json(admin.request, `${url}/aperio/api/exposes/policies/${org.id}`, 'PUT', policy)
    const actions = ['read', 'create', 'update', 'enable', 'disable', 'delete', 'disconnect']
    await json(admin.request, `${url}/aperio/api/orgs/select`, 'POST', { id: org.id })
    const user = await json(admin.request, `${url}/aperio/api/users`, 'POST', { username: 'publisher', password,
      grants: [{ org: org.id, role: 'viewer', expose: actions, expose_bounds: { ...policy, revision: 1 } }] })
    await json(admin.request, `${url}/aperio/api/users`, 'POST', { username: 'plain', password, role: 'viewer' })
    const tunnelToken = await json(admin.request, `${url}/aperio/api/tokens`, 'POST', { name: 'declaring-client' })
    const config = join(dir, 'client.yaml')
    await writeFile(config, `server:\n  url: ${url}\n  token: ${tunnelToken.token}\nconnections: 1\ntunnels:\n  - name: echo\n    target: 127.0.0.1:${backendPort}\n    protocol: tcp/udp\n`)
    run('aperio-client', ['--config', config], { APERIO_CONNECTIONS: '1' })
    await login(publisher, url, 'publisher', password); await login(plain, url, 'plain', password)
    const page = await publisher.newPage(); await page.goto(`${url}/aperio/?tab=tunnels`)
    await expect(page.getByRole('row').filter({ hasText: `127.0.0.1:${backendPort}` })).toBeVisible({ timeout: 30000 })
    for (const protocol of ['tcp', 'udp']) {
      await page.getByRole('button', { name: 'Create public expose', exact: true }).click()
      const dialog = page.getByRole('dialog')
      await dialog.getByLabel('Tunnel name', { exact: true }).fill('echo')
      await dialog.getByLabel('Protocol', { exact: true }).selectOption(protocol)
      await dialog.getByLabel('Listen address', { exact: true }).fill('127.0.0.1')
      await dialog.getByLabel('Port', { exact: true }).fill(String(publicPort))
      await dialog.getByLabel('Enabled', { exact: true }).check()
      await dialog.getByLabel('I understand that this endpoint accepts public traffic.').check()
      await dialog.getByRole('button', { name: 'Save', exact: true }).click()
      await expect(dialog).toBeHidden()
    }
    const items = (await json(publisher.request, `${url}/aperio/api/exposes`)).items
    expect(items).toHaveLength(2)
    const tcp = items.find((i: any) => i.resource.spec.listener.protocol === 'tcp').resource
    const udp = items.find((i: any) => i.resource.spec.listener.protocol === 'udp').resource
    expect(await tcpEcho(publicPort, 'tcp-live')).toBe('tcp-live'); expect(await udpEcho(publicPort, 'udp-live')).toBe('udp-live')
    const udpRow = page.getByRole('row').filter({ hasText: `udp://127.0.0.1:${publicPort}` })
    await expect(udpRow.getByRole('button', { name: '1 sessions', exact: true })).toBeVisible()
    await udpRow.getByRole('button', { name: '1 sessions', exact: true }).click()
    await expect(page.getByRole('dialog')).toContainText(`127.0.0.1:${backendPort}`)
    await page.getByRole('dialog').getByRole('button', { name: 'Close', exact: true }).click()
    const topology = await json(publisher.request, `${url}/aperio/api/topology`)
    expect(topology.exposes.map((e: any) => e.id).sort()).toEqual([tcp.id, udp.id].sort())
    expect((await plain.request.get(`${url}/aperio/api/exposes/${tcp.id}`)).status()).toBe(404)
    expect((await plain.request.post(`${url}/aperio/api/exposes`, { data: { spec: tcp.spec } })).status()).toBe(403)
    expect((await publisher.request.post(`${url}/aperio/api/exposes`, { data: { spec: { ...tcp.spec, org_id: other.id } } })).status()).toBe(403)
    const updated = await json(publisher.request, `${url}/aperio/api/exposes/${tcp.id}`, 'PUT', { revision: tcp.revision, spec: tcp.spec })
    expect((await publisher.request.put(`${url}/aperio/api/exposes/${tcp.id}`, { data: { revision: tcp.revision, spec: tcp.spec } })).status()).toBe(409)
    expect(updated.revision).toBe(tcp.revision + 1)
    const scoped = await json(admin.request, `${url}/aperio/api/admin-keys`, 'POST', { name: 'scoped', role: 'viewer', org_id: org.id, expose: actions, expose_bounds: { ...policy, revision: 1 } })
    const keyContext = await browser.newContext({ extraHTTPHeaders: { authorization: `Bearer ${scoped.key}` } })
    try { expect((await json(keyContext.request, `${url}/aperio/api/exposes`)).total).toBe(2) } finally { await keyContext.close() }
    await stop(server); startServer(); await waitServer(); await login(publisher, url, 'publisher', password)
    await expect.poll(async () => (await json(publisher.request, `${url}/aperio/api/exposes`)).items.filter((i: any) => i.resource.target_state === 'ready').length, { timeout: 30000 }).toBe(2)
    expect(await tcpEcho(publicPort, 'after-restart')).toBe('after-restart'); expect(await udpEcho(publicPort, 'after-restart')).toBe('after-restart')
    // Conflicts and offline targets use the same non-master control surface.
    const conflicting = await publisher.request.post(`${url}/aperio/api/exposes`, { data: { spec: tcp.spec } })
    expect(conflicting.status()).toBe(409); expect((await conflicting.json()).code).toBe('port_conflict')
    const offline = await json(publisher.request, `${url}/aperio/api/exposes`, 'POST', { spec: { ...tcp.spec, tunnel: 'offline', enabled: false } })
    expect((await json(publisher.request, `${url}/aperio/api/exposes/${offline.id}`)).target_state).toBe('waiting')
    await json(publisher.request, `${url}/aperio/api/exposes/${offline.id}`, 'DELETE', { revision: offline.revision })
    await page.reload()
    const restartedUdp = page.getByRole('row').filter({ hasText: `udp://127.0.0.1:${publicPort}` })
    await restartedUdp.getByRole('button', { name: 'Drain', exact: true }).click()
    await page.getByRole('dialog').getByLabel('Drain timeout (seconds)', { exact: true }).fill('1')
    await page.getByRole('dialog').getByRole('button', { name: 'Drain', exact: true }).click()
    await expect.poll(async () => (await json(publisher.request, `${url}/aperio/api/exposes/${udp.id}`)).state).toBe('disabled')
    expect(await tcpEcho(publicPort, 'while-udp-drained')).toBe('while-udp-drained')
    await restartedUdp.getByRole('button', { name: 'Enable', exact: true }).click()
    await page.getByRole('dialog').getByRole('button', { name: 'Enable', exact: true }).click()
    expect(await udpEcho(publicPort, 're-enabled')).toBe('re-enabled')
    await login(admin, url, 'aperio', token)
    await json(admin.request, `${url}/aperio/api/exposes/policies/${org.id}`, 'PUT', { ...policy, revision: 1, allocations: [] })
    await expect.poll(async () => (await json(publisher.request, `${url}/aperio/api/exposes`)).items.filter((i: any) => i.resource.state === 'suspended').length).toBe(2)
    await json(admin.request, `${url}/aperio/api/exposes/policies/${org.id}`, 'PUT', { ...policy, revision: 2 })
    expect(await tcpEcho(publicPort, 'policy-restored')).toBe('policy-restored')
    await json(admin.request, `${url}/aperio/api/orgs/select`, 'POST', { id: org.id })
    await json(admin.request, `${url}/aperio/api/users/${user.id}`, 'PUT', { grants: [{ org: org.id, role: 'viewer', expose: [] }] })
    expect((await publisher.request.get(`${url}/aperio/api/exposes/${tcp.id}`)).status()).toBe(404)
    await expect(page.getByRole('button', { name: 'Create public expose', exact: true })).toHaveCount(0, { timeout: 15000 })
    expect(await tcpEcho(publicPort, 'org-owned')).toBe('org-owned')
    for (const id of [tcp.id, udp.id]) {
      const current = await json(admin.request, `${url}/aperio/api/exposes/${id}`)
      await json(admin.request, `${url}/aperio/api/exposes/${id}`, 'DELETE', { revision: current.revision })
    }
    expect((await json(admin.request, `${url}/aperio/api/exposes`)).total).toBe(0)
  } finally {
    await publisher.close(); await plain.close(); await admin.close()
    for (const proc of processes.reverse()) await stop(proc)
    for (const socket of sockets) socket.destroy()
    datagrams.close(); await new Promise<void>((done) => backend.close(() => done()))
  }
})
