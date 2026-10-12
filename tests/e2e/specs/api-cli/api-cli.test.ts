import { Test } from 'nole'
import assert from 'node:assert/strict'
import { mkdtempSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { AperioServerBase } from '../../lib/server.js'
import { StandardBackendBase } from '../../lib/backend.js'
import { ClientFor } from '../../lib/client.js'
import { api, apiWithoutKey, client } from '../../lib/cli.js'
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
    // `*` Admin: the CLI walks the server-wide surfaces too, which are the
    // server admin's, and master's Admin is only master's.
    const minted = await this.server._api<{ key: string }>('/aperio/api/admin-keys', {
      method: 'POST',
      body: JSON.stringify({ name: 'e2e-cli', role: 'admin', org_id: '*' }),
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
      '--topic', 'deploy/#', '--topic', '*', '--allow-bind',
    )
    assert.match(made.token, /^apr_/, 'the secret is returned once')

    type Listed = { id: string; name: string; topics: string[]; allow_bind: boolean; allow_server_side: boolean }
    const listed = () => this._json<Listed[]>('token', 'list').then((all) => all.find((t) => t.id === made.id)!)
    let row = await listed()
    assert.equal(row.name, 'e2e-cli-token')
    // The server keeps what the flags said: `*` is stored as `#`, the
    // messaging spelling of everything, and the capability is on.
    assert.deepEqual(row.topics, ['deploy/#', '#'])
    assert.equal(row.allow_bind, true)
    assert.equal(row.allow_server_side, false)

    await this._api('token', 'update', made.id, '--name', 'e2e-cli-renamed', '--topic', 'deploy/web', '--no-allow-bind')
    row = await listed()
    assert.equal(row.name, 'e2e-cli-renamed')
    assert.deepEqual(row.topics, ['deploy/web'], 'a --topic list replaces the old one')
    assert.equal(row.allow_bind, false)

    await this._api('token', 'update', made.id, '--clear-topics')
    row = await listed()
    assert.deepEqual(row.topics, [], 'and --clear-topics empties it')

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

  async signingInOnceKeepsAKeyForAdminCalls() {
    // A home of its own: the profile file is the thing under test, and the
    // machine's own must be neither read nor written.
    const home = mkdtempSync(join(tmpdir(), 'aperio-login-'))
    const env = {
      APERIO_SERVER_URL: this.server._url,
      APERIO_SERVER_API_KEY: '',
      APERIO_SERVER_TOKEN: '',
      HOME: home,
      USERPROFILE: home,
    }
    try {
      // The built-in account, whose password is the master token.
      const login = await client(['login'], { ...env, APERIO_SERVER_TOKEN: this.server._token })
      assert.ok(login.ok, `login failed: ${login.stderr}`)

      // No key anywhere on the command line or in the environment: the
      // profile carries it, and `admin` is `api`.
      const tokens = await client(['admin', 'token', 'list'], env)
      assert.ok(tokens.ok, `admin token list failed: ${tokens.stderr}`)
      const keys = await client(['admin', 'admin-key', 'list'], env)
      assert.ok(keys.ok, keys.stderr)
      const minted = (JSON.parse(keys.stdout) as { name: string }[]).filter((k) =>
        k.name.startsWith('login '),
      )
      assert.equal(minted.length, 1, 'login minted one key for this machine')

      const profiles = await client(['profile', 'list'], env)
      assert.match(profiles.stdout, /^\* /m, 'the new profile is in use')

      // Signing out revokes what signing in minted.
      const logout = await client(['logout'], env)
      assert.ok(logout.ok, logout.stderr)
      assert.match(logout.stderr, /revoked/)
      const after = await client(['admin', 'token', 'list'], env)
      assert.equal(after.ok, false, 'signed out, the call has no credential')
      const left = await this._json<{ name: string }[]>('admin-key', 'list')
      assert.ok(!left.some((k) => k.name.startsWith('login ')), 'the key is gone on the server too')
    } finally {
      rmSync(home, { recursive: true, force: true })
    }
  }
}
