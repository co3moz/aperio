import { Test } from 'nole'
import assert from 'node:assert/strict'
import { AperioServerBase } from '../../lib/server.js'
import { StandardBackendBase } from '../../lib/backend.js'
import { AperioClientBase } from '../../lib/client.js'
import { waitFor } from '../../lib/env.js'

/** Random subdomains off, so a hostname that is not routed really is not. */
export class MultiplexServer extends AperioServerBase({
  env: { APERIO_RANDOM_SUBDOMAIN: '' },
}) {}

export class WebBackend extends StandardBackendBase() {}
export class ApiBackend extends StandardBackendBase() {}

/**
 * One client, two services, one WebSocket between them.
 *
 * The point of the fixture is the pair of `multiplex: true` lines. Everything
 * else is the ordinary two-service shape, which is what makes the assertions
 * below meaningful: the services are configured exactly as they would be on
 * two connections, and the only question is whether they still behave that way
 * on one.
 */
export class MultiplexedClient extends AperioClientBase({
  dependencies: {
    server: () => MultiplexServer,
    web: () => WebBackend,
    api: () => ApiBackend,
  },
}) {
  _serverUrl() {
    return this.server._url
  }
  _serverToken() {
    return this.server._token
  }
  _hostname() {
    return 'mx-web.e2e.local'
  }
  _readyPath() {
    return '/hello'
  }
  _config() {
    return [
      'server:',
      `  url: ${this.server._url}`,
      `  token: ${this.server._token}`,
      'multiplex: true',
      'services:',
      '  - name: mx_web',
      `    target: ${this.web._url}`,
      '    hostname: mx-web.e2e.local',
      '  - name: mx_api',
      `    target: ${this.api._url}`,
      '    hostname: mx-api.e2e.local',
      '    max_concurrent: 4',
      '',
    ].join('\n')
  }
}

interface ClientRow {
  id: string
  service_index: number
  service: string | null
  declared_hostnames: string[]
  max_concurrent: number | null
}

export class MultiplexSpec extends Test({
  timeout: 120_000,
  dependencies: {
    server: () => MultiplexServer,
    web: () => WebBackend,
    api: () => ApiBackend,
    client: () => MultiplexedClient,
  },
}) {
  async _rows(): Promise<ClientRow[]> {
    const stats = await this.server._api<{ active_clients: ClientRow[] }>('/aperio/api/stats')
    return stats.active_clients.filter((c) => c.service?.startsWith('mx_'))
  }

  async bothServicesRouteToTheirOwnBackend() {
    await waitFor(async () => (await this._rows()).length === 2, {
      label: 'both services to be declared',
    })
    // The whole feature in two requests: one socket, and each hostname still
    // reaches the backend its own entry names.
    const web = await this.server._fetch('/hello', { host: 'mx-web.e2e.local' })
    assert.equal(web.body, `backend ${this.web._port} GET /hello`)
    const api = await this.server._fetch('/hello', { host: 'mx-api.e2e.local' })
    assert.equal(api.body, `backend ${this.api._port} GET /hello`)
  }

  async theTwoServicesShareOneConnection() {
    const rows = await this._rows()
    // One connection id, two service rows on it. Two ids here would mean the
    // client had quietly fallen back to a connection per service, and every
    // other assertion in this file would still pass.
    assert.equal(new Set(rows.map((r) => r.id)).size, 1)
    assert.deepEqual(
      rows.map((r) => r.service_index).sort(),
      [0, 1],
    )
  }

  async eachServiceKeepsItsOwnSettings() {
    const rows = await this._rows()
    const web = rows.find((r) => r.service === 'mx_web')
    const api = rows.find((r) => r.service === 'mx_api')
    assert.deepEqual(web?.declared_hostnames, ['mx-web.e2e.local'])
    assert.deepEqual(api?.declared_hostnames, ['mx-api.e2e.local'])
    // Read from the second entry, which is the one a connection-level value
    // would have got wrong: `max_concurrent` is only written on `mx_api`.
    assert.equal(web?.max_concurrent, null)
    assert.equal(api?.max_concurrent, 4)
  }

  async anUnclaimedHostnameIsStillNotRouted() {
    const res = await this.server._fetch('/hello', { host: 'mx-nope.e2e.local' })
    assert.equal(res.status, 504)
  }
}
