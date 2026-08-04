import { Test } from 'nole'
import assert from 'node:assert/strict'
import { readFile } from 'node:fs/promises'
import { waitFor } from '../../lib/env.js'
import { send, sendRaw } from '../../lib/http.js'
import { totp } from '../../lib/totp.js'
import { BaseServerFor, BaseBackendFor, BaseClientFor, HOST } from './fixtures.js'

/** This file's own server: the specs below change it, so it is not
 *  shared with another file. See `fixtures.ts`. */
class AdminServer extends BaseServerFor() {}
class AdminBackend extends BaseBackendFor() {}
class AdminClient extends BaseClientFor(() => AdminServer, () => AdminBackend) {}

export class AuditApiSpec extends Test({
  timeout: 60_000,
  dependencies: { server: () => AdminServer, client: () => AdminClient },
}) {
  /** Makes its own event rather than leaning on another spec having made one:
   *  a class that only passes when its neighbours ran is a class nobody can
   *  run on its own to find out what broke. */
  async before() {
    const hook = await this.server._api<{ id: string }>('/aperio/api/webhooks', {
      method: 'POST',
      body: JSON.stringify({
        name: 'e2e-audit-hook',
        url: 'http://127.0.0.1:1/',
        events: ['token_created'],
      }),
    })
    const cookie = await this.server._login()
    await this.server._fetch(`/aperio/api/webhooks/${hook.id}`, {
      method: 'DELETE',
      headers: { cookie },
    })
  }

  async theLogRecordsWhatHappenedAndWhoDidIt() {
    const audit = await this.server._api<{ event: string; actor: string }[]>('/aperio/api/audit')
    assert.ok(audit.some((e) => e.event === 'client_connected'))
    assert.ok(audit.some((e) => e.event === 'webhook_created'))
    assert.ok(audit.some((e) => e.actor === 'aperio'), 'dashboard actions name the user')
    assert.ok(audit.some((e) => e.actor === 'system'), 'system events are attributed to system')
  }

  async anEventFilterNarrowsToOneKind() {
    // Matched on the event field rather than as a substring: a
    // webhook_created entry legitimately names the events it subscribed to in
    // its details, so a loose search would fail on a correct answer.
    const filtered = await this.server._api<{ event: string }[]>(
      '/aperio/api/audit?event=webhook_created',
    )
    assert.ok(filtered.length > 0)
    assert.ok(filtered.every((e) => e.event === 'webhook_created'))
  }

  async aSubstringSearchCoversTheDetails() {
    const found = await this.server._api<unknown[]>('/aperio/api/audit?q=webhook')
    assert.ok(found.length > 0)
  }

  async aRangeThatExcludesEverythingReturnsNothing() {
    // Empty rather than unfiltered: the filter is really applied, not
    // silently dropped when it matches nothing.
    const future = await this.server._api<unknown[]>('/aperio/api/audit?from=2999-01-01')
    assert.deepEqual(future, [])
  }

  async theRequestLogTakesTheSameShapeOfQuery() {
    const logs = await this.server._api<{ method: string }[]>('/aperio/api/logs?method=GET&limit=5')
    assert.ok(logs.every((l) => l.method === 'GET'))
    assert.ok(logs.length <= 5, 'the limit was ignored')
  }

  async theCsvExportAnswersTheSameQuery() {
    const cookie = await this.server._login()
    const res = await this.server._fetch('/aperio/api/export/audit.csv?event=webhook_created', {
      headers: { cookie },
    })
    assert.match(res.headers['content-type'] ?? '', /text\/csv/)
    assert.match(res.headers['content-disposition'] ?? '', /aperio-audit\.csv/)
    assert.match(res.body, /^timestamp,ts,event,actor,actor_ip,org,details/m)
    assert.match(res.body, /webhook_created/)
  }

  async theOpenApiDocumentIsServedToASession() {
    const spec = await this.server._api<{ openapi: string; paths: Record<string, unknown> }>(
      '/aperio/api/openapi.json',
    )
    assert.ok(spec.openapi)
    assert.ok('/aperio/api/tokens/refresh' in spec.paths)
    assert.equal((await this.server._fetch('/aperio/api/openapi.json')).status, 302)
  }
}

export class RoleBasedAccessSpec extends Test({
  // Ordered: this file's specs share one server and change it, so they
  // take turns. Files do not, they have a server each.
  after: () => [AuditApiSpec],
  timeout: 90_000,
  dependencies: { server: () => AdminServer },
}) {
  async _signIn(user: string, password: string) {
    const cookies = await sendRaw(this.server._url, '/aperio/auth', {
      method: 'POST',
      headers: {
        authorization: `Basic ${Buffer.from(`${user}:${password}`).toString('base64')}`,
      },
    })
    return cookies.at(0)?.split(';')[0] ?? null
  }

  async aViewerMayReadAndNothingElse() {
    const user = await this.server._api<{ id: string; role: string; username: string }>(
      '/aperio/api/users',
      {
        method: 'POST',
        body: JSON.stringify({
          username: 'e2e-viewer',
          password: 'viewer-password',
          role: 'viewer',
        }),
      },
    )
    assert.equal(user.role, 'viewer')
    assert.equal(user.username, 'e2e-viewer')

    const cookie = await this._signIn('e2e-viewer', 'viewer-password')
    assert.ok(cookie, 'the viewer can sign in')

    const session = await this.server._json<{ role: string }>('/aperio/api/session', {
      headers: { cookie },
    })
    assert.equal(session.role, 'viewer')

    assert.equal((await this.server._fetch('/aperio/api/stats', { headers: { cookie } })).status, 200)
    const created = await this.server._fetch('/aperio/api/tokens', {
      method: 'POST',
      headers: { cookie, 'content-type': 'application/json' },
      body: JSON.stringify({ name: 'nope', hostnames: ['*'] }),
    })
    assert.equal(created.status, 403, 'creating a token needs operator')
    assert.equal((await this.server._fetch('/aperio/api/users', { headers: { cookie } })).status, 403)
    assert.equal(
      (await this.server._fetch('/aperio/api/settings', { headers: { cookie } })).status,
      403,
    )

    const adminCookie = await this.server._login()
    const deleted = await this.server._fetch(`/aperio/api/users/${user.id}`, {
      method: 'DELETE',
      headers: { cookie: adminCookie },
    })
    assert.equal(deleted.status, 200)
    assert.equal(await this._signIn('e2e-viewer', 'viewer-password'), null)
  }

  async aShortPasswordIsRejected() {
    const cookie = await this.server._login()
    const res = await this.server._fetch('/aperio/api/users', {
      method: 'POST',
      headers: { cookie, 'content-type': 'application/json' },
      body: JSON.stringify({ username: 'e2e-x', password: 'short', role: 'viewer' }),
    })
    assert.equal(res.status, 400)
  }
}

export class TotpSpec extends Test({
  // Ordered: this file's specs share one server and change it, so they
  // take turns. Files do not, they have a server each.
  after: () => [RoleBasedAccessSpec],
  timeout: 90_000,
  dependencies: { server: () => AdminServer },
}) {
  async _auth(headers: Record<string, string> = {}) {
    return send(this.server._url, '/aperio/auth', {
      method: 'POST',
      headers: {
        authorization: `Basic ${Buffer.from('e2e-mfa:mfa-password').toString('base64')}`,
        ...headers,
      },
    })
  }

  async theSecondFactorIsEnrolledVerifiedAndResettable() {
    const user = await this.server._api<{ id: string }>('/aperio/api/users', {
      method: 'POST',
      body: JSON.stringify({ username: 'e2e-mfa', password: 'mfa-password', role: 'operator' }),
    })

    // Before enrollment, the password alone is enough.
    assert.equal((await this._auth()).status, 200)
    const cookies = await sendRaw(this.server._url, '/aperio/auth', {
      method: 'POST',
      headers: {
        authorization: `Basic ${Buffer.from('e2e-mfa:mfa-password').toString('base64')}`,
      },
    })
    const cookie = cookies.at(0)?.split(';')[0] ?? ''

    const setup = await this.server._json<{ secret: string; url: string }>(
      '/aperio/api/me/totp/setup',
      { method: 'POST', headers: { cookie } },
    )
    assert.ok(setup.secret)
    assert.match(JSON.stringify(setup), /otpauth:\/\/totp\/Aperio:e2e-mfa/)

    const wrong = await this.server._fetch('/aperio/api/me/totp/enable', {
      method: 'POST',
      headers: { cookie, 'content-type': 'application/json' },
      body: JSON.stringify({ code: '000000' }),
    })
    assert.equal(wrong.status, 400, 'a wrong code does not complete enrollment')

    const enabled = await this.server._json<{ recovery_codes: string[] }>(
      '/aperio/api/me/totp/enable',
      {
        method: 'POST',
        headers: { cookie, 'content-type': 'application/json' },
        body: JSON.stringify({ code: totp(setup.secret) }),
      },
    )
    assert.ok(enabled.recovery_codes.length > 0)
    const recovery = enabled.recovery_codes[0]

    const users = await this.server._api<{ username: string; totp: boolean }[]>('/aperio/api/users')
    assert.equal(users.find((u) => u.username === 'e2e-mfa')?.totp, true)

    // Password alone now asks for the second factor, without a session.
    const refused = await this._auth()
    assert.equal(refused.status, 401)
    assert.equal(refused.headers['x-aperio-totp'], 'required')

    assert.equal((await this._auth({ 'x-aperio-totp': '000000' })).status, 401)
    assert.equal((await this._auth({ 'x-aperio-totp': totp(setup.secret) })).status, 200)

    // A recovery code works exactly once.
    assert.equal((await this._auth({ 'x-aperio-totp': recovery })).status, 200)
    assert.equal((await this._auth({ 'x-aperio-totp': recovery })).status, 401)

    const adminCookie = await this.server._login()
    const reset = await this.server._fetch(`/aperio/api/users/${user.id}/totp`, {
      method: 'DELETE',
      headers: { cookie: adminCookie },
    })
    assert.equal(reset.status, 200)
    assert.equal((await this._auth()).status, 200, 'password-only works again after the reset')

    const removed = await this.server._fetch(`/aperio/api/users/${user.id}`, {
      method: 'DELETE',
      headers: { cookie: adminCookie },
    })
    assert.equal(removed.status, 200)
  }
}

export class PasskeySurfaceSpec extends Test({
  // Ordered: this file's specs share one server and change it, so they
  // take turns. Files do not, they have a server each.
  after: () => [TotpSpec],
  timeout: 60_000,
  dependencies: { server: () => AdminServer },
}) {
  async theCeremoniesAnswer501WithoutAnOrigin() {
    const probe = await this.server._json<{ available: boolean }>('/aperio/auth/passkey')
    assert.equal(probe.available, false)

    const start = await this.server._fetch('/aperio/auth/passkey/start', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ username: 'nobody' }),
    })
    assert.equal(start.status, 501)

    const cookie = await this.server._login()
    const register = await this.server._fetch('/aperio/api/me/passkeys/register/start', {
      method: 'POST',
      headers: { cookie },
    })
    assert.equal(register.status, 501)
  }
}

export class AccessLogSpec extends Test({
  // Ordered: this file's specs share one server and change it, so they
  // take turns. Files do not, they have a server each.
  after: () => [PasskeySurfaceSpec],
  timeout: 60_000,
  dependencies: {
    server: () => AdminServer,
    backend: () => AdminBackend,
    client: () => AdminClient,
  },
}) {
  async itRecordsProxiedRequestsAndAttributesTheToken() {
    await this.server._fetch('/hello', { host: HOST })
    await waitFor(
      async () => (await readFile(this.server._accessLog(), 'utf8')).includes('"uri":"/hello'),
      { label: 'the access log to be written' },
    )
    const log = await readFile(this.server._accessLog(), 'utf8')
    assert.match(log, /"token":"master"/)
  }
}

export class TokenLifecycleSpec extends Test({
  // Ordered: this file's specs share one server and change it, so they
  // take turns. Files do not, they have a server each.
  after: () => [AccessLogSpec],
  timeout: 60_000,
  dependencies: { server: () => AdminServer },
}) {
  async _status(path: string, method: string, body?: unknown): Promise<number> {
    const cookie = await this.server._login()
    const res = await this.server._fetch(path, {
      method,
      headers: { cookie, 'content-type': 'application/json' },
      body: body === undefined ? undefined : JSON.stringify(body),
    })
    return res.status
  }

  async aTokenIsCreatedEditedAndRevokedExactlyOnce() {
    const token = await this.server._mintToken({
      name: 'e2e-edit',
      hostnames: ['edit.e2e.local'],
    })
    const list = await this.server._api<{ name: string }[]>('/aperio/api/tokens')
    assert.ok(list.some((t) => t.name === 'e2e-edit'))

    assert.equal(
      await this._status(`/aperio/api/tokens/${token.id}`, 'PUT', {
        hostnames: ['edited.e2e.local'],
        max_rps: 5,
        ttl_seconds: 600,
        allow_public: true,
      }),
      200,
    )
    const edited = await this.server._api<{ hostnames: string[]; allow_public: boolean }[]>(
      '/aperio/api/tokens',
    )
    const row = edited.find((t) => t.hostnames.includes('edited.e2e.local'))
    assert.ok(row, 'the edit updated the hostname scope')
    assert.equal(row.allow_public, true)

    assert.equal(
      await this._status(`/aperio/api/tokens/${token.id}`, 'PUT', { hostnames: ['bad host'] }),
      400,
    )
    assert.equal(await this._status('/aperio/api/tokens/no-such-token', 'PUT', { name: 'x' }), 404)
    assert.equal(await this._status(`/aperio/api/tokens/${token.id}`, 'DELETE'), 200)
    assert.equal(await this._status(`/aperio/api/tokens/${token.id}`, 'DELETE'), 404)
  }
}

export class ClientControlSpec extends Test({
  // Ordered: this file's specs share one server and change it, so they
  // take turns. Files do not, they have a server each.
  after: () => [TokenLifecycleSpec],
  timeout: 90_000,
  dependencies: {
    server: () => AdminServer,
    backend: () => AdminBackend,
    client: () => AdminClient,
  },
}) {
  async _clientId(): Promise<string> {
    const stats = await this.server._api<{ active_clients: { id: string }[] }>('/aperio/api/stats')
    assert.equal(stats.active_clients.length, 1, 'exactly one client should be connected here')
    return stats.active_clients[0].id
  }

  async _status(path: string, method: string, body?: unknown): Promise<number> {
    const cookie = await this.server._login()
    const res = await this.server._fetch(path, {
      method,
      headers: { cookie, 'content-type': 'application/json' },
      body: body === undefined ? undefined : JSON.stringify(body),
    })
    return res.status
  }

  async anOverrideIsValidatedAppliedAndCleared() {
    const id = await this._clientId()
    assert.equal(
      await this._status(`/aperio/api/clients/${id}/override`, 'POST', {
        hostname_bind: 'bad host',
      }),
      400,
    )
    assert.equal(
      await this._status('/aperio/api/clients/no-such-client/override', 'POST', {
        path_bind: '/x',
      }),
      404,
    )

    assert.equal(
      await this._status(`/aperio/api/clients/${id}/override`, 'POST', { path_bind: '/ov' }),
      200,
    )
    let stats = await this.server._api<{ active_clients: Record<string, unknown>[] }>(
      '/aperio/api/stats',
    )
    assert.equal(stats.active_clients[0].override_path_bind, '/ov')

    // The list form moves one bind at a time, the dashboard's overrule dialog.
    assert.equal(
      await this._status(`/aperio/api/clients/${id}/override`, 'POST', {
        hostname_binds: ['moved.example.com', 'kept.example.com'],
      }),
      200,
    )
    stats = await this.server._api<{ active_clients: Record<string, unknown>[] }>(
      '/aperio/api/stats',
    )
    assert.deepEqual(stats.active_clients[0].override_hostname_binds, [
      'moved.example.com',
      'kept.example.com',
    ])

    assert.equal(
      await this._status(`/aperio/api/clients/${id}/override`, 'POST', {
        path_bind: '',
        hostname_bind: '',
        hostname_binds: [],
      }),
      200,
    )
  }

  async theConfigViewDescribesTheConnection() {
    const id = await this._clientId()
    const view = await this.server._api<{ yaml: string }>(`/aperio/api/clients/${id}/config`)
    assert.match(view.yaml, /Effective configuration of connection/)
    assert.match(view.yaml, new RegExp(HOST))
    assert.equal(await this._status('/aperio/api/clients/no-such-client/config', 'GET'), 404)
  }

  async theKillSwitchTakesTheClientOutOfRoutingAndBack() {
    const id = await this._clientId()
    assert.equal(
      await this._status(`/aperio/api/clients/${id}/enabled`, 'POST', { enabled: false }),
      200,
    )
    assert.equal((await this.server._fetch('/hello', { host: HOST })).status, 504)

    assert.equal(
      await this._status('/aperio/api/clients/no-such-client/enabled', 'POST', { enabled: true }),
      404,
    )
    assert.equal(
      await this._status(`/aperio/api/clients/${id}/enabled`, 'POST', { enabled: true }),
      200,
    )
    await waitFor(
      async () =>
        (await this.server._fetch('/hello', { host: HOST })).body ===
        `backend ${this.backend._port} GET /hello`,
      { label: 'traffic to resume' },
    )
  }
}
