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

/** A server whose gate is written as a list: a login for people, a key for
 *  scripts. The case the grammar exists for. */
export class BearerServer extends AperioServerBase() {
  _configFile() {
    return [
      'server:',
      '  auth:',
      '    - method: basic',
      '      users: "demo:secret123"',
      '    - method: bearer',
      '      secret: "0123456789abcdef-e2e-secret"',
      '      query: true',
      '',
    ].join('\n')
  }
}

export class BearerBackend extends StandardBackendBase() {}

export class BearerClient extends ClientFor(() => BearerServer, () => BearerBackend) {
  /** No routability wait: every path on this host is gated, so the 401 the
   *  gate answers is what a probe would see. The first phase waits instead. */
  _hostname() {
    return ''
  }
  _env() {
    return { APERIO_HOSTNAME: 'bearer.e2e.local' }
  }
}

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

/**
 * The `auth:` grammar written as method blocks rather than as the scalar.
 * Both services belong to one client, so the file also proves the two
 * spellings coexist in one `services:` list.
 */
export class MethodGrammarClient extends AuthClient {
  _autoStart() {
    return false
  }
  _config() {
    return [
      'server:',
      `  url: ${this.server._url}`,
      `  token: ${this.server._token}`,
      'services:',
      '  - name: gated_by_block',
      `    target: ${this._backendUrl()}`,
      '    hostname: block.e2e.local',
      '    auth: { method: basic, users: "block:blockpw" }',
      '  - name: open_by_method',
      `    target: ${this._backendUrl()}`,
      '    hostname: open.e2e.local',
      '    auth: { method: none }',
      '',
    ].join('\n')
  }
}

/** A method this build does not know must stop the client, not be ignored. */
export class UnknownMethodClient extends AuthClient {
  _autoStart() {
    return false
  }
  _config() {
    return [
      'server:',
      `  url: ${this.server._url}`,
      `  token: ${this.server._token}`,
      'services:',
      '  - name: unknown_method',
      `    target: ${this._backendUrl()}`,
      '    hostname: unknown.e2e.local',
      '    auth: { method: ldap }',
      '',
    ].join('\n')
  }
}

export class AuthMethodGrammarSpec extends Test({
  timeout: 90_000,
  after: () => [VisitorGateSpec],
  dependencies: {
    server: () => AuthServer,
    backend: () => AuthBackend,
    grammar: () => MethodGrammarClient,
    unknown: () => UnknownMethodClient,
  },
}) {
  async before() {
    await this.grammar._start()
    // Waiting on the open service answering is the routability wait for both:
    // they are two connections of one process and come up together. The gated
    // one cannot be waited on the same way, since its 302 is also what an
    // unrouted hostname produces while the server's own gate is configured.
    await waitFor(
      async () => {
        const res = await this.server._fetch('/hello', { host: 'open.e2e.local' })
        return res.body === `backend ${this.backend._port} GET /hello`
      },
      { label: 'the grammar client to be routable' },
    )
  }

  async aBlockSpelledBasicGatesWithItsOwnCredentials() {
    const res = await this.server._fetch('/hello', { host: 'block.e2e.local' })
    assert.equal(res.status, 302, 'the block form has to gate like the scalar')

    // The client's own credentials open it, and the server's do not: a
    // client-declared gate supersedes the server's for that route, which is
    // the rule the scalar spelling already followed.
    const open = await this.server._fetch('/aperio/auth?redirect=/hello', {
      host: 'block.e2e.local',
      headers: { authorization: `Basic ${Buffer.from('block:blockpw').toString('base64')}` },
      method: 'POST',
    })
    assert.ok(
      open.status < 400,
      `the client's own credentials should log in, got ${open.status}`,
    )
    const wrong = await this.server._fetch('/aperio/auth?redirect=/hello', {
      host: 'block.e2e.local',
      headers: { authorization: `Basic ${Buffer.from('demo:secret123').toString('base64')}` },
      method: 'POST',
    })
    assert.equal(wrong.status, 401, "the server's own password must not open a client's gate")
  }

  async methodNoneIsTheLongSpellingOfPublic() {
    // No login, no redirect: `{method: none}` reached the server as the same
    // declaration `public: true` makes, and the sibling service on the very
    // same connection stays gated.
    const res = await this.server._fetch('/hello', { host: 'open.e2e.local' })
    assert.equal(res.status, 200)
    assert.equal(res.body, `backend ${this.backend._port} GET /hello`)
  }

  async aMethodThisBuildDoesNotKnowStopsTheClient() {
    await this.unknown._start().catch(() => {})
    // Named, and naming what does exist: an operator who wrote a gate must
    // never be left with no gate and no reason.
    await this.unknown._waitForLog('is not a method')
    await this.unknown._waitForLog('basic')
  }
}

const BEARER_SECRET = '0123456789abcdef-e2e-secret'
const BEARER_HOST = 'bearer.e2e.local'

/** The gate a script can reach, which is the case that had no answer at all. */
export class BearerMethodSpec extends Test({
  timeout: 90_000,
  dependencies: {
    server: () => BearerServer,
    backend: () => BearerBackend,
    client: () => BearerClient,
  },
}) {
  async aScriptPresentsTheSecretInAHeaderAndIsServed() {
    await waitFor(
      async () => {
        const res = await this.server._fetch('/hello', {
          host: BEARER_HOST,
          headers: { authorization: `Bearer ${BEARER_SECRET}` },
        })
        return res.body === `backend ${this.backend._port} GET /hello`
      },
      { label: 'the bearer gate to admit the secret' },
    )
  }

  async aScriptWithoutTheSecretIsToldWhatToPresent() {
    // Not a redirect to an HTML login form: that is what made a gated route
    // unreachable with curl in the first place.
    const res = await this.server._fetch('/hello', { host: BEARER_HOST })
    assert.equal(res.status, 401)
    assert.equal(res.headers['www-authenticate'], 'Bearer')

    const wrong = await this.server._fetch('/hello', {
      host: BEARER_HOST,
      headers: { authorization: 'Bearer not-the-secret-at-all' },
    })
    assert.equal(wrong.status, 401)
  }

  async aBrowserOnTheSameGateStillGetsTheLoginPage() {
    const res = await this.server._fetch('/hello', {
      host: BEARER_HOST,
      headers: { accept: 'text/html' },
    })
    assert.equal(res.status, 302)
    assert.match(res.headers['location'] ?? '', /\/aperio\/auth/)
  }

  async theListStillAdmitsThePersonWithAPassword() {
    // Any-of, in practice: the same route, the other method.
    const res = await this.server._fetch('/aperio/auth?redirect=/hello', {
      host: BEARER_HOST,
      method: 'POST',
      headers: { authorization: `Basic ${Buffer.from('demo:secret123').toString('base64')}` },
    })
    assert.ok(res.status < 400, `the password should log in, got ${res.status}`)
  }

  async aPageOpenedWithTheSecretInItsUrlIsSentToACleanAddress() {
    const res = await this.server._fetch(`/hello?aperio_token=${BEARER_SECRET}&page=2`, {
      host: BEARER_HOST,
      headers: { accept: 'text/html' },
    })
    assert.equal(res.status, 302)
    assert.equal(res.headers['location'], '/hello?page=2')
    assert.match(res.headers['set-cookie'] ?? '', /aperio_share=/)
  }

  async theQueryFormServesANonNavigationDirectly() {
    // An <img src> or a sender that cannot set headers: no redirect, and the
    // parameter never reaches the backend, which echoes what it was asked for.
    const res = await this.server._fetch(`/hello?aperio_token=${BEARER_SECRET}`, {
      host: BEARER_HOST,
    })
    assert.equal(res.status, 200)
    assert.equal(res.body, `backend ${this.backend._port} GET /hello`)
  }
}

/** A server that serves what is declared reachable, and nothing else. */
export class ClosedServer extends AperioServerBase({
  env: { APERIO_DEFAULT_ACCESS: 'deny' },
}) {}

export class ClosedBackend extends StandardBackendBase() {}

/** Declares nothing: under `deny` this is exactly what is refused. */
export class UndeclaredClient extends ClientFor(() => ClosedServer, () => ClosedBackend) {
  _hostname() {
    return ''
  }
  _env() {
    return { APERIO_HOSTNAME: 'undeclared.e2e.local' }
  }
}

/** Says it is open, which is the sentence that serves a route under `deny`. */
export class DeclaredOpenClient extends ClientFor(() => ClosedServer, () => ClosedBackend) {
  _hostname() {
    return 'declared.e2e.local'
  }
  _readyPath() {
    return '/hello'
  }
  _env() {
    return { APERIO_HOSTNAME: 'declared.e2e.local', APERIO_PUBLIC: '1' }
  }
}

export class ClosedByDefaultSpec extends Test({
  timeout: 90_000,
  dependencies: {
    server: () => ClosedServer,
    backend: () => ClosedBackend,
    undeclared: () => UndeclaredClient,
    declared: () => DeclaredOpenClient,
  },
}) {
  async whatDeclaresItselfOpenIsServed() {
    const res = await this.server._fetch('/hello', { host: 'declared.e2e.local' })
    assert.equal(res.status, 200)
    assert.equal(res.body, `backend ${this.backend._port} GET /hello`)
  }

  async whatDeclaresNothingIsRefusedIndistinguishablyFromAnUnclaimedRoute() {
    // Both answers are the same on purpose: a caller who was never going to
    // be let in learns nothing about whether anything is there.
    const undeclared = await this.server._fetch('/hello', { host: 'undeclared.e2e.local' })
    const nobody = await this.server._fetch('/hello', { host: 'nothing-here.e2e.local' })
    assert.equal(undeclared.status, 504)
    assert.equal(undeclared.status, nobody.status)
    assert.equal(undeclared.body, nobody.body)
  }
}
