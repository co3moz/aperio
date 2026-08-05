import { Test } from 'nole'
import assert from 'node:assert/strict'
import { spawn, type ChildProcess } from 'node:child_process'
import { createServer, type Server } from 'node:http'
import { AperioServerBase } from '../../lib/server.js'
import { StandardBackendBase } from '../../lib/backend.js'
import { AperioClientBase, terminate } from '../../lib/client.js'
import { CLIENT_BIN, freePort, waitFor } from '../../lib/env.js'

/**
 * The provider's scale endpoint, mocked.
 *
 * It records every call and, on the first one, starts the very client Aperio
 * is waiting for, which is what a real cold start does. `GET` is the
 * readiness probe and must never start anything, or the test would be
 * watching a client it started itself.
 */
export class ScaleHook extends Test({
  timeout: 60_000,
  dependencies: { backend: () => ScalingBackend },
}) {
  _port = 0
  _calls: Record<string, unknown>[] = []
  _server?: Server
  /** What a cold start runs. Returns the process it started, so this class
   *  can stop it: a hook that starts a client and forgets it leaks one per
   *  run, which is exactly what this used to do. */
  _startClient: (() => ChildProcess) | null = null
  private _started = false
  private _spawned: ChildProcess[] = []

  async hookListen() {
    this._port = await freePort()
    this._server = createServer((req, res) => {
      if (req.method !== 'POST') {
        res.writeHead(200, { 'content-length': '2' }).end('up')
        return
      }
      const chunks: Buffer[] = []
      req.on('data', (c: Buffer) => chunks.push(c))
      req.on('end', () => {
        this._calls.push(JSON.parse(Buffer.concat(chunks).toString() || '{}'))
        // Only the first call starts an instance. A correct server makes
        // exactly one anyway; this keeps the test honest if it does not.
        if (!this._started && this._startClient) {
          this._started = true
          this._spawned.push(this._startClient())
        }
        res.writeHead(200, { 'content-length': '2' }).end('ok')
      })
    })
    await new Promise<void>((resolve) => this._server!.listen(this._port, '127.0.0.1', resolve))
  }

  _url(): string {
    return `http://127.0.0.1:${this._port}/scale`
  }

  async cleanUp() {
    // The clients this hook cold-started first: they hold a tunnel to a
    // server that is about to go away, and nobody else knows they exist.
    await Promise.all(this._spawned.map(terminate))
    this._spawned = []
    await new Promise<void>((resolve) => {
      if (!this._server) return resolve()
      this._server.closeAllConnections()
      this._server.close(() => resolve())
    })
  }
}

export class ScalingBackend extends StandardBackendBase() {}

/** Loopback is refused by the SSRF fence by default; this server opts in,
 *  which is what an internal provider API needs. The strict default has its
 *  own server below. */
export class ScalingServer extends AperioServerBase({
  env: {
    APERIO_SCALING: '1',
    APERIO_SCALING_ALLOW_HTTP: '1',
    APERIO_SCALING_ALLOW_PRIVATE: '1',
    APERIO_GATEWAY_TIMEOUT: '2',
  },
}) {}

export class StrictScalingServer extends AperioServerBase({
  env: { APERIO_SCALING: '1', APERIO_GATEWAY_TIMEOUT: '2' },
}) {}

/** Connects once to arm the record, then goes away. */
class ArmingClient extends AperioClientBase({
  dependencies: {
    server: () => ScalingServer,
    backend: () => ScalingBackend,
    hook: () => ScaleHook,
  },
}) {
  _host = 'scale.e2e.local'

  _autoStart() {
    return false
  }
  _serverUrl() {
    return this.server._url
  }
  _serverToken() {
    return this.server._token
  }
  _config() {
    return [
      'server:',
      `  url: ${this.server._url}`,
      `  token: ${this.server._token}`,
      `target: ${this.backend._url}`,
      `hostname: ${this._host}`,
      'max_concurrent: 10',
      'scaling:',
      `  url: ${this.hook._url()}`,
      '  min: 0',
      '  max: 4',
      '  cold_start: 30s',
      '  cooldown: 1s',
      '',
    ].join('\n')
  }
}

export class ScaleArmingClient extends ArmingClient {}

export class StrictArmingClient extends AperioClientBase({
  dependencies: {
    server: () => StrictScalingServer,
    backend: () => ScalingBackend,
    hook: () => ScaleHook,
  },
}) {
  _autoStart() {
    return false
  }
  _serverUrl() {
    return this.server._url
  }
  _serverToken() {
    return this.server._token
  }
  _config() {
    return [
      'server:',
      `  url: ${this.server._url}`,
      `  token: ${this.server._token}`,
      `target: ${this.backend._url}`,
      'hostname: ssrf.e2e.local',
      'scaling:',
      `  url: ${this.hook._url()}`,
      '  min: 0',
      '  max: 4',
      '  cold_start: 30s',
      '  cooldown: 1s',
      '',
    ].join('\n')
  }
}

interface ClientView {
  hostname_binds?: string[] | null
}

export class ColdStartSpec extends Test({
  timeout: 180_000,
  dependencies: {
    server: () => ScalingServer,
    backend: () => ScalingBackend,
    hook: () => ScaleHook,
    arming: () => ScaleArmingClient,
  },
}) {
  static recordId = ''

  /** Gone means absent from the *live* client list. Never probed with a real
   *  request: that request would trigger the cold start under test. */
  async _gone(host: string): Promise<boolean> {
    const stats = await this.server._api<{ active_clients: ClientView[] }>('/aperio/api/stats')
    return !stats.active_clients.some((c) => (c.hostname_binds ?? []).includes(host))
  }

  async anArmedRecordOutlivesTheClientThatArmedIt() {
    // What the hook starts when the server asks for capacity.
    this.hook._startClient = () =>
      spawn(CLIENT_BIN, {
        env: {
          ...process.env,
          APERIO_CONNECTIONS: '1',
          APERIO_SERVER_URL: this.server._url,
          APERIO_SERVER_TOKEN: this.server._token,
          APERIO_TARGET: this.backend._url,
          APERIO_HOSTNAME: 'scale.e2e.local',
        },
        stdio: ['ignore', 'ignore', 'ignore'],
      })

    await this.arming._start()
    await this.arming._waitRoutable('scale.e2e.local', '/hello')
    await this.arming._kill()
    await waitFor(() => this._gone('scale.e2e.local'), {
      label: 'the armed client to leave routing',
    })
  }

  async aRequestColdStartsTheServiceInsteadOfFailing() {
    // Nothing serves the hostname: the server has to call the endpoint and
    // hold this request until what it starts becomes routable.
    const res = await this.server._fetch('/hello', { host: 'scale.e2e.local' })
    assert.match(res.body, new RegExp(`^backend ${this.backend._port} `))

    const call = this.hook._calls.at(0)
    assert.ok(call, 'the endpoint was never called')
    assert.equal(call.reason, 'cold_start')
    assert.equal(call.hostname, 'scale.e2e.local')
    assert.equal(call.desired, 1, 'a cold start asks for one instance')
    assert.equal(this.hook._calls.length, 1, 'a burst produces exactly one call')
  }

  async theScalingApiReportsTheArmedRecord() {
    const records = await this.server._api<
      { id: string; hostname: string; authenticated: boolean; instances: number }[]
    >('/aperio/api/scaling')
    const record = records.find((r) => r.hostname === 'scale.e2e.local')
    assert.ok(record, `no record in ${JSON.stringify(records)}`)
    assert.equal(record.authenticated, false, 'no secret was declared')
    assert.equal(record.instances, 1, 'the live pool is reported')
    ColdStartSpec.recordId = record.id
  }

  async disarmingRemovesIt() {
    const cookie = await this.server._login()
    const gone = await this.server._fetch(
      `/aperio/api/scaling/${encodeURIComponent(ColdStartSpec.recordId)}`,
      { method: 'DELETE', headers: { cookie } },
    )
    assert.equal(gone.status, 200)

    const unknown = await this.server._fetch('/aperio/api/scaling/no-such-record', {
      method: 'DELETE',
      headers: { cookie },
    })
    assert.equal(unknown.status, 404)
  }
}

/** The strict default: an internal endpoint is refused before any request
 *  leaves the process, and the visitor is not held for the cold-start budget
 *  waiting for something that will never start. */
export class SsrfFenceSpec extends Test({
  timeout: 180_000,
  dependencies: {
    server: () => StrictScalingServer,
    backend: () => ScalingBackend,
    hook: () => ScaleHook,
    arming: () => StrictArmingClient,
  },
}) {
  async _gone(host: string): Promise<boolean> {
    const stats = await this.server._api<{ active_clients: ClientView[] }>('/aperio/api/stats')
    return !stats.active_clients.some((c) => (c.hostname_binds ?? []).includes(host))
  }

  async aRefusedEndpointFallsThroughToTheNormal504WithoutHoldingTheVisitor() {
    await this.arming._start()
    await this.arming._waitRoutable('ssrf.e2e.local', '/hello')
    await this.arming._kill()
    await waitFor(() => this._gone('ssrf.e2e.local'), { label: 'the client to leave routing' })

    const started = Date.now()
    const res = await this.server._fetch('/hello', { host: 'ssrf.e2e.local' })
    const elapsed = Date.now() - started
    assert.equal(res.status, 504)
    assert.ok(elapsed < 25_000, `a refused call held the visitor for ${elapsed}ms`)
    await this.server._waitForLog('Scaling: call for ssrf.e2e.local failed')
  }
}
