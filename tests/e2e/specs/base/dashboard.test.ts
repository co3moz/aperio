import { Test } from 'nole'
import assert from 'node:assert/strict'
import { readFile } from 'node:fs/promises'
import { join } from 'node:path'
import { waitFor } from '../../lib/env.js'
import { sendRaw } from '../../lib/http.js'
import { ClientFor } from '../../lib/client.js'
import { BaseServerFor, BaseBackendFor, BaseClientFor, HOST, METRICS_TOKEN } from './fixtures.js'

/** This file's own server: the specs below change it, so it is not
 *  shared with another file. See `fixtures.ts`. */
class DashboardServer extends BaseServerFor() {}
class DashboardBackend extends BaseBackendFor() {}
class DashboardClient extends BaseClientFor(() => DashboardServer, () => DashboardBackend) {}

/** A client for the ephemeral tunnel the API mints. */
export class EphemeralClient extends ClientFor(() => DashboardServer, () => DashboardBackend) {
  _token = ''
  _autoStart() {
    return false
  }
  _serverToken() {
    return this._token
  }
}

export class DashboardApiSpec extends Test({
  timeout: 90_000,
  dependencies: {
    server: () => DashboardServer,
    backend: () => DashboardBackend,
    client: () => DashboardClient,
  },
}) {
  async aBadPasswordIsRejected() {
    const cookies = await sendRaw(this.server._url, '/aperio/auth', {
      method: 'POST',
      headers: {
        authorization: `Basic ${Buffer.from('aperio:wrong-password').toString('base64')}`,
      },
    })
    assert.equal(cookies.length, 0, 'a rejected login sets no session')
  }

  async theStatsShowTheConnectedClientAndItsTraffic() {
    const stats = await this.server._api<{
      connected_clients_count: number
      active_clients: { hostname_binds?: string[] | null }[]
      persistent: { by_hostname: Record<string, unknown> }
    }>('/aperio/api/stats')
    assert.equal(stats.connected_clients_count, 1)
    assert.ok(stats.active_clients.some((c) => (c.hostname_binds ?? []).includes(HOST)))
    assert.ok(stats.persistent.by_hostname, 'the traffic breakdown is included')
  }

  async theHistoryEndpointBucketsAndValidatesItsRange() {
    const week = await this.server._api<{ period: string; requests: number }[]>(
      '/aperio/api/stats/history?unit=day&count=7',
    )
    assert.equal(week.length, 7, 'seven day buckets')
    assert.equal(typeof week[0].requests, 'number')

    const cookie = await this.server._login()
    const unknownUnit = await this.server._fetch('/aperio/api/stats/history?unit=fortnight', {
      headers: { cookie },
    })
    assert.equal(unknownUnit.status, 400)
    const reversed = await this.server._fetch(
      '/aperio/api/stats/history?from=2026-02-02&to=2026-01-01',
      { headers: { cookie } },
    )
    assert.equal(reversed.status, 400)
  }

  async theUptimeHistoryReportsTheConnectedClient() {
    await waitFor(
      async () => {
        const rows = await this.server._api<{ status: string }[]>('/aperio/api/uptime')
        return rows.some((r) => r.status === 'up')
      },
      { label: 'the client to be reported up' },
    )
    const rows = await this.server._api<{ pct_today: unknown; days: unknown[] }[]>(
      '/aperio/api/uptime',
    )
    // `pct_today` is null until a day has something in it, so the claim is
    // that the field is there, not that it already has a number in it.
    assert.ok('pct_today' in rows[0], 'entries carry a percentage field')
    assert.ok(Array.isArray(rows[0].days), 'entries carry daily buckets')
  }

  async theTopologyCoversEverySurfaceAndNeedsASession() {
    const topo = await this.server._api<{
      clients: { hostname_binds?: string[] | null }[]
      routes: unknown[]
      exposes: unknown[]
      offline: unknown[]
    }>('/aperio/api/topology')
    assert.ok(topo.clients.some((c) => (c.hostname_binds ?? []).includes(HOST)))
    for (const key of ['routes', 'exposes', 'offline'] as const) {
      assert.ok(Array.isArray(topo[key]), `${key} is missing`)
    }
    assert.equal((await this.server._fetch('/aperio/api/topology')).status, 302)
  }

  async theRequestLogCapturedTheProxiedPost() {
    // Makes the request it then looks for. It used to read a POST another
    // file had sent through the server they shared, which is why this is the
    // one spec that noticed when they stopped sharing it.
    const sent = await this.server._fetch('/submit', {
      host: HOST,
      method: 'POST',
      headers: { 'content-type': 'text/plain' },
      body: 'payload-123',
    })
    assert.equal(sent.status, 200)

    const logs = await this.server._api<{ uri: string }[]>('/aperio/api/logs')
    assert.ok(logs.some((l) => l.uri.startsWith('/submit')))
    assert.equal((await this.server._fetch('/aperio/api/stats')).status, 302)
  }
}

interface Settings {
  effective: Record<string, unknown>
  environment: unknown
}

export class SettingsApiSpec extends Test({
  // Ordered: this file's specs share one server and change it, so they
  // take turns. Files do not, they have a server each.
  after: () => [DashboardApiSpec],
  timeout: 60_000,
  dependencies: { server: () => DashboardServer },
}) {
  async _put(body: unknown): Promise<number> {
    const cookie = await this.server._login()
    const res = await this.server._fetch('/aperio/api/settings', {
      method: 'PUT',
      headers: { cookie, 'content-type': 'application/json' },
      body: JSON.stringify(body),
    })
    return res.status
  }

  async anUpdateAppliesLiveAndIsPersisted() {
    const before = await this.server._api<Settings>('/aperio/api/settings')
    assert.equal(before.effective.lb_strategy, 'round-robin')

    assert.equal(await this._put({ gateway_timeout_secs: 5, lb_strategy: 'sticky' }), 200)
    const after = await this.server._api<Settings>('/aperio/api/settings')
    assert.equal(after.effective.lb_strategy, 'sticky')

    const persisted = await readFile(join(this.server._dataDir, 'settings.json'), 'utf8')
    assert.match(persisted, /"gateway_timeout_secs": 5/)
  }

  async theReportNamesTheEnvOnlyFlagsAndTheDefaults() {
    const settings = await this.server._api<Settings>('/aperio/api/settings')
    assert.ok(settings.environment, 'the env-only flag report is exposed')
    assert.match(JSON.stringify(settings.environment), /APERIO_TRUST_PROXY/)
    assert.equal(typeof settings.effective.cache_enabled, 'boolean')
    assert.equal(settings.effective.ui_language, 'en')
  }

  async theRuntimeSettingsApplyLive() {
    assert.equal(
      await this._put({
        cache_enabled: true,
        max_concurrent_requests: 64,
        login_lockout_threshold: 7,
      }),
      200,
    )
    const settings = await this.server._api<Settings>('/aperio/api/settings')
    assert.equal(settings.effective.max_concurrent_requests, 64)
  }

  async invalidValuesAreRejectedAndOverridesCanBeReset() {
    assert.equal(await this._put({ lb_strategy: 'bogus' }), 400)
    assert.equal(await this._put({ cache_max_bytes: 0 }), 400, 'a zero cache budget is refused')
    assert.equal(await this._put({}), 200, 'overrides can be reset')
  }
}

export class ProgrammaticTunnelsSpec extends Test({
  // Ordered: this file's specs share one server and change it, so they
  // take turns. Files do not, they have a server each.
  after: () => [SettingsApiSpec],
  timeout: 90_000,
  dependencies: {
    server: () => DashboardServer,
    backend: () => DashboardBackend,
    ephemeral: () => EphemeralClient,
  },
}) {
  async provisioningNeedsACredential() {
    const res = await this.server._fetch('/aperio/api/tunnels', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: '{}',
    })
    assert.equal(res.status, 401)
  }

  async anEphemeralTunnelProxiesAndCanBeRevokedExactlyOnce() {
    const auth = {
      authorization: `Bearer ${this.server._token}`,
      'content-type': 'application/json',
    }
    const tunnel = await this.server._json<{ token: string; hostname: string; id: string }>(
      '/aperio/api/tunnels',
      { method: 'POST', headers: auth, body: JSON.stringify({ name: 'e2e-preview', ttl_seconds: 300 }) },
    )
    assert.match(tunnel.token, /^apr_/, 'the response carries an ephemeral token')
    assert.match(tunnel.hostname, /\.e2e\.local$/, 'and a random subdomain')

    this.ephemeral._token = tunnel.token
    await this.ephemeral._start()
    await this.ephemeral._waitRoutable(tunnel.hostname, '/preview')
    const res = await this.server._fetch('/preview', { host: tunnel.hostname })
    assert.equal(res.body, `backend ${this.backend._port} GET /preview`)

    const revoked = await this.server._fetch(`/aperio/api/tunnels/${tunnel.id}`, {
      method: 'DELETE',
      headers: { authorization: `Bearer ${this.server._token}` },
    })
    assert.equal(revoked.status, 200)
    const again = await this.server._fetch(`/aperio/api/tunnels/${tunnel.id}`, {
      method: 'DELETE',
      headers: { authorization: `Bearer ${this.server._token}` },
    })
    assert.equal(again.status, 404)

    // Leave only the main client connected: the client-control spec reads the
    // first active client and would otherwise disable this one.
    await this.ephemeral._kill()
    await this.server._waitForClients(1)
  }
}

export class MetricsSpec extends Test({
  // Ordered: this file's specs share one server and change it, so they
  // take turns. Files do not, they have a server each.
  after: () => [ProgrammaticTunnelsSpec],
  timeout: 60_000,
  dependencies: { server: () => DashboardServer, client: () => DashboardClient },
}) {
  async aScrapeNeedsItsTokenAndAcceptsEitherForm() {
    assert.equal((await this.server._fetch('/aperio/metrics')).status, 401)

    const query = await this.server._fetch(`/aperio/metrics?token=${METRICS_TOKEN}`)
    assert.equal(query.status, 200)
    for (const family of [
      'aperio_requests_total',
      'aperio_connected_clients',
      'aperio_request_duration_seconds_bucket',
    ]) {
      assert.match(query.body, new RegExp(family), family)
    }
    assert.match(query.body, /aperio_token_requests_total\{token="/)
    assert.match(query.body, /aperio_hostname_requests_total\{hostname="/)

    const bearer = await this.server._fetch('/aperio/metrics', {
      headers: { authorization: `Bearer ${METRICS_TOKEN}` },
    })
    assert.match(bearer.body, /aperio_requests_total/)
  }
}
