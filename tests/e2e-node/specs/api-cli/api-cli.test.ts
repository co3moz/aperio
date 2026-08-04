import { Test } from 'nole'
import assert from 'node:assert/strict'
import { AperioServerBase } from '../../lib/server.js'
import { StandardBackendBase } from '../../lib/backend.js'
import { ClientFor } from '../../lib/client.js'
import { api, apiWithoutKey } from '../../lib/cli.js'
import { waitFor } from '../../lib/env.js'

const HOST = 'app.e2e.local'

export class CliServer extends AperioServerBase() {}
export class CliBackend extends StandardBackendBase() {}

export class CliClient extends ClientFor(() => CliServer, () => CliBackend) {
  _hostname() {
    return HOST
  }
  _readyPath() {
    return '/hello'
  }
  _env() {
    return { APERIO_HOSTNAME: HOST }
  }
}

/**
 * The `aperio-client api …` commands, which authenticate with a programmatic
 * admin key rather than a dashboard session.
 */
export class ApiCliSpec extends Test({
  timeout: 120_000,
  dependencies: {
    server: () => CliServer,
    backend: () => CliBackend,
    client: () => CliClient,
  },
}) {
  _key = ''

  async before() {
    const minted = await this.server._api<{ key: string }>('/aperio/api/admin-keys', {
      method: 'POST',
      body: JSON.stringify({ name: 'e2e-cli', role: 'admin' }),
    })
    this._key = minted.key
  }

  async _api(...args: string[]) {
    const res = await api(this.server._url, this._key, args)
    assert.ok(res.ok, `api ${args.join(' ')} failed: ${res.stderr || res.stdout}`)
    return res.stdout
  }

  async _json<T>(...args: string[]): Promise<T> {
    return JSON.parse(await this._api(...args)) as T
  }

  async theReadOnlyReportsAnswer() {
    assert.match(await this._api('stats'), /"active_clients"/)
    const health = await this._json<{ status: string }>('health')
    assert.equal(health.status, 'healthy')
    assert.match(await this._api('topology'), new RegExp(HOST))
    // CSV, not JSON: the point of the command.
    assert.match(await this._api('traffic-csv', '--count', '2'), /period,requests/)
  }

  async shareLinksAreMintedAndValidated() {
    const link = await this._json<{ url: string }>(
      'share', '--hostname', HOST, '--path', '/test', '--expire', '1d',
    )
    assert.match(link.url, /aperio_share=/, 'the link carries the signed token')

    // `never` is a real value, not an omitted field.
    const forever = await this._json<{ expires_at: number | null }>(
      'share', '--hostname', HOST, '--expire', 'never',
    )
    assert.equal(forever.expires_at, null)

    const bad = await api(this.server._url, this._key, [
      'share', '--hostname', HOST, '--expire', 'tomorrow',
    ])
    assert.equal(bad.ok, false, 'an invalid duration is rejected before any request')
  }

  async theTokenLifecycleWorksEndToEnd() {
    const made = await this._json<{ token: string; id: string }>(
      'token', 'create', '--name', 'e2e-cli-token', '--hostname', HOST, '--expire', '1d',
    )
    assert.match(made.token, /^apr_/, 'the secret is returned once')

    assert.match(await this._api('token', 'list'), /e2e-cli-token/)
    await this._api('token', 'update', made.id, '--name', 'e2e-cli-renamed')
    assert.match(await this._api('token', 'list'), /e2e-cli-renamed/)

    const rotated = await this._json<{ token: string }>('token', 'rotate', made.id, '--grace', '1h')
    assert.match(rotated.token, /^apr_/)

    await this._api('token', 'revoke', made.id)
    assert.doesNotMatch(await this._api('token', 'list'), /e2e-cli-renamed/)
  }

  async maintenanceModeFlagsAndUnflagsAHost() {
    await this._api('maintenance', 'on', HOST)
    assert.match(await this._api('maintenance', 'list'), new RegExp(HOST))
    const flagged = await this.server._fetch('/hello', { host: HOST })
    assert.equal(flagged.status, 503)

    await this._api('maintenance', 'off', HOST)
    await waitFor(
      async () => (await this.server._fetch('/hello', { host: HOST })).status === 200,
      { label: 'routing to come back' },
    )
  }

  async anEphemeralTunnelIsMintedAndDeleted() {
    const made = await this._json<{ token: string; id: string }>(
      'tunnel', 'create', '--name', 'e2e-cli-tunnel', '--hostname', 'cli.e2e.local',
      '--expire', '30m',
    )
    assert.match(made.token, /^apr_/, 'it mints a scoped token')
    await this._api('tunnel', 'delete', made.id)
  }

  async usersWebhooksAndTheCacheAreManageable() {
    const user = await this._json<{ role: string; id: string }>(
      'user', 'create', '--username', 'e2e-cli-user', '--password', 'e2e-cli-password',
      '--role', 'operator',
    )
    assert.equal(user.role, 'operator')
    await this._api('user', 'update', user.id, '--role', 'viewer')
    assert.match(await this._api('user', 'list'), /"role": "viewer"/)
    await this._api('user', 'delete', user.id)

    const hook = await this._json<{ status: string; id: string }>(
      'webhook', 'create', '--name', 'e2e-cli-hook', '--url', 'http://127.0.0.1:1/none',
      '--event', 'client_connected',
    )
    assert.equal(hook.status, 'ok')
    await this._api('webhook', 'delete', hook.id)

    assert.match(await this._api('cache', 'purge'), /"removed"/)
  }

  async aCallWithoutAnAdminKeyFails() {
    const res = await apiWithoutKey(this.server._url, ['stats'])
    // Fails, rather than succeeding with the login page as its "answer".
    assert.equal(res.ok, false)
  }
}
