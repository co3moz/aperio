import { Test } from 'nole'
import assert from 'node:assert/strict'
import { AperioServerBase } from '../../lib/server.js'
import { StandardBackendBase } from '../../lib/backend.js'
import { ClientFor } from '../../lib/client.js'
import { waitFor } from '../../lib/env.js'

const HOST = 'app.e2e.local'

export class AuthServer extends AperioServerBase({
  env: { APERIO_SERVER_AUTH: 'demo:secret123' },
}) {}

export class AuthBackend extends StandardBackendBase() {}

class AuthClient extends ClientFor(() => AuthServer, () => AuthBackend) {}

/** The gated service. No routability wait: the login page would answer it. */
export class GatedClient extends AuthClient {
  _env() {
    return { APERIO_HOSTNAME: HOST }
  }
}

/** `public: true` on the master token, which may always publish public. */
export class PublicClient extends AuthClient {
  _autoStart() {
    return false
  }
  _env() {
    return { APERIO_HOSTNAME: 'pub.e2e.local', APERIO_PUBLIC: '1' }
  }
}

/** Asks to be public with a token that was never granted it. */
export class UnpermittedPublicClient extends AuthClient {
  _token = ''
  _autoStart() {
    return false
  }
  _serverToken() {
    return this._token
  }
  _env() {
    return { APERIO_HOSTNAME: 'priv.e2e.local', APERIO_PUBLIC: '1' }
  }
}

/** Connects with a secret that has been rotated away from. */
export class RotatedClient extends AuthClient {
  _token = ''
  _host = 'rot.e2e.local'
  _autoStart() {
    return false
  }
  _serverToken() {
    return this._token
  }
  _env() {
    return { APERIO_HOSTNAME: this._host }
  }
}

export class StaleSecretClient extends RotatedClient {}

export class VisitorGateSpec extends Test({
  timeout: 90_000,
  dependencies: {
    server: () => AuthServer,
    backend: () => AuthBackend,
    gated: () => GatedClient,
  },
}) {
  static shareToken = ''

  async anUnauthenticatedVisitorIsSentToTheLoginPage() {
    await this.server._waitForClients(1)
    const res = await this.server._fetch('/hello', { host: HOST })
    assert.equal(res.status, 302)
    assert.match(res.headers['location'] ?? '', /\/aperio\/auth/)
  }

  async aShareLinkRedirectsToTheCleanUrlAndSetsItsCookie() {
    const share = await this.server._api<{ token: string }>('/aperio/api/share', {
      method: 'POST',
      body: JSON.stringify({ hostname: HOST, ttl_seconds: 300 }),
    })
    assert.ok(share.token)
    VisitorGateSpec.shareToken = share.token

    const res = await this.server._fetch(`/hello?aperio_share=${share.token}`, { host: HOST })
    assert.equal(res.status, 302)
    assert.match(res.headers['set-cookie'] ?? '', /aperio_share=/)
    assert.equal(res.headers['location'], '/hello')
  }

  async theShareCookieGrantsAccess() {
    const cookie = `aperio_share=${VisitorGateSpec.shareToken}`
    // The hostname bind arrives with the client's first heartbeat, so this
    // doubles as the routability wait the login page would otherwise mask.
    await waitFor(
      async () => {
        const res = await this.server._fetch('/hello', { host: HOST, headers: { cookie } })
        return res.body === `backend ${this.backend._port} GET /hello`
      },
      { label: 'the share cookie to grant access' },
    )
  }

  async theShareCookieDoesNotCoverAnotherHostname() {
    const res = await this.server._fetch('/hello', {
      host: 'other.e2e.local',
      headers: { cookie: `aperio_share=${VisitorGateSpec.shareToken}` },
    })
    assert.equal(res.status, 302)
  }

  async aTamperedShareTokenIsRejected() {
    const res = await this.server._fetch('/hello?aperio_share=tampered.token', { host: HOST })
    assert.match(res.headers['location'] ?? '', /\/aperio\/auth/)
  }
}

export class PublicServiceSpec extends Test({
  timeout: 90_000,
  after: () => [VisitorGateSpec],
  dependencies: {
    server: () => AuthServer,
    backend: () => AuthBackend,
    gated: () => GatedClient,
    open: () => PublicClient,
    unpermitted: () => UnpermittedPublicClient,
  },
}) {
  async aPublicServiceServesWithoutLogin() {
    await this.open._start()
    await waitFor(
      async () => {
        const res = await this.server._fetch('/hello', { host: 'pub.e2e.local' })
        return res.body === `backend ${this.backend._port} GET /hello`
      },
      { label: 'the public service to bypass the gate' },
    )
  }

  async theProtectedHostnameKeepsItsGate() {
    const res = await this.server._fetch('/hello', { host: HOST })
    assert.match(res.headers['location'] ?? '', /\/aperio\/auth/)
  }

  async aTokenWithoutAllowPublicCannotOpenAService() {
    const minted = await this.server._mintToken({
      name: 'nopublic',
      hostnames: ['priv.e2e.local'],
    })
    this.unpermitted._token = minted.token
    await this.unpermitted._start()
    await this.server._waitForLog('does not permit publishing public')

    const res = await this.server._fetch('/hello', { host: 'priv.e2e.local' })
    assert.match(res.headers['location'] ?? '', /\/aperio\/auth/, 'the gate stays on')
  }
}

export class TokenRotationSpec extends Test({
  timeout: 90_000,
  after: () => [VisitorGateSpec],
  dependencies: {
    server: () => AuthServer,
    backend: () => AuthBackend,
    graced: () => RotatedClient,
    stale: () => StaleSecretClient,
  },
}) {
  async bothSecretsWorkWhileTheGraceWindowIsOpen() {
    const first = await this.server._mintToken({ name: 'rotate-me', hostnames: ['rot.e2e.local'] })
    const rotated = await this.server._api<{ token: string }>(
      `/aperio/api/tokens/${first.id}/rotate`,
      { method: 'POST', body: JSON.stringify({ grace_seconds: 3600 }) },
    )
    assert.ok(rotated.token)
    assert.notEqual(rotated.token, first.token, 'rotation mints a new secret')

    // The pre-rotation secret still connects.
    this.graced._token = first.token
    await this.graced._start()
    await this.graced._waitRoutable('rot.e2e.local', '/hello')

    // And an immediate cutover ends it for anything connecting afterwards.
    await this.server._api(`/aperio/api/tokens/${first.id}/rotate`, {
      method: 'POST',
      body: JSON.stringify({ grace_seconds: 0 }),
    })
    this.stale._token = rotated.token
    this.stale._host = 'rot2.e2e.local'
    await this.stale._start()
    await waitFor(() => /invalid token|401|unauthorized/i.test(this.stale._log()), {
      label: 'the client to be told its secret is gone',
    })
  }
}
