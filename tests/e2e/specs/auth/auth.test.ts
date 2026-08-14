import { Test } from 'nole'
import assert from 'node:assert/strict'
import { AperioServerBase } from '../../lib/server.js'
import { StandardBackendBase } from '../../lib/backend.js'
import { ClientFor } from '../../lib/client.js'
import { waitFor } from '../../lib/env.js'
import { createHmac } from 'node:crypto'
import { createServer } from 'node:http'

const HOST = 'app.e2e.local'

export class AuthServer extends AperioServerBase({
  env: { APERIO_SERVER_AUTH: 'demo:secret123' },
}) {}

/** A server whose gate is written as a list: a login for people, a key for
 *  scripts. The case the grammar exists for. */
export class BearerServer extends AperioServerBase({
  env: { APERIO_VISITOR_IDENTITY_HEADERS: '1' },
}) {
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
    assert.match(
      res.headers['location'] ?? '',
      /\/aperio\/auth/,
      `status ${res.status} body ${res.body.slice(0, 200)}`,
    )
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

  async theBackendIsToldWhoCameIn() {
    // The gate knew and never said, which is why an application behind a
    // tunnel had to build a second login to greet anyone.
    const res = await this.server._fetch('/echo-headers', {
      host: BEARER_HOST,
      headers: { authorization: `Bearer ${BEARER_SECRET}` },
    })
    assert.equal(res.status, 200)
    assert.match(res.body, /x-aperio-visitor-how: bearer/)
    // A secret identifies a caller and not a person, so there is no id, and
    // the secret itself never travels onward.
    assert.doesNotMatch(res.body, /x-aperio-visitor-id/)
    assert.doesNotMatch(res.body, new RegExp(BEARER_SECRET))
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

const JWT_SECRET = '0123456789abcdef-e2e-jwt-secret'
const JWT_HOST = 'jwt.e2e.local'

/** Minimal HS256 signing, so the suite needs no JWT library of its own. */
function signHs256(claims: Record<string, unknown>): string {
  const b64 = (buf: Buffer) => buf.toString('base64url')
  const head = b64(Buffer.from(JSON.stringify({ alg: 'HS256', typ: 'JWT' })))
  const body = b64(Buffer.from(JSON.stringify(claims)))
  const mac = createHmac('sha256', JWT_SECRET).update(`${head}.${body}`).digest()
  return `${head}.${body}.${b64(mac)}`
}

export class JwtServer extends AperioServerBase({
  env: { APERIO_VISITOR_IDENTITY_HEADERS: '1' },
}) {
  _configFile() {
    return [
      'server:',
      '  auth:',
      '    - method: jwt',
      `      hmac_secret: "${JWT_SECRET}"`,
      '      issuer: https://accounts.e2e.local',
      '      audience: aperio',
      '      claims: { groups: engineering }',
      '',
    ].join('\n')
  }
}

export class JwtBackend extends StandardBackendBase() {}

export class JwtClient extends ClientFor(() => JwtServer, () => JwtBackend) {
  _hostname() {
    return ''
  }
  _env() {
    return { APERIO_HOSTNAME: JWT_HOST }
  }
}

export class JwtMethodSpec extends Test({
  timeout: 90_000,
  dependencies: {
    server: () => JwtServer,
    backend: () => JwtBackend,
    client: () => JwtClient,
  },
}) {
  _exp = 0

  async before() {
    this._exp = Math.floor(Date.now() / 1000) + 600
  }

  _token(extra: Record<string, unknown> = {}) {
    return signHs256({
      sub: 'u-1',
      email: 'alice@e2e.local',
      iss: 'https://accounts.e2e.local',
      aud: 'aperio',
      groups: 'engineering',
      exp: this._exp,
      ...extra,
    })
  }

  async aGoodTokenIsServedAndNamesItsHolder() {
    await waitFor(
      async () => {
        const res = await this.server._fetch('/echo-headers', {
          host: JWT_HOST,
          headers: { authorization: `Bearer ${this._token()}` },
        })
        return res.status === 200
      },
      { label: 'the jwt gate to admit a good token' },
    )
    const res = await this.server._fetch('/echo-headers', {
      host: JWT_HOST,
      headers: { authorization: `Bearer ${this._token()}` },
    })
    assert.match(res.body, /x-aperio-visitor-how: jwt/)
    assert.match(res.body, /x-aperio-visitor-id: alice@e2e\.local/)
    // Aperio's credential does not travel on to the backend.
    assert.doesNotMatch(res.body, /authorization:/)
  }

  async aTokenFailingAnyOneRuleIsRefused() {
    for (const [why, extra] of [
      ['a claim that does not match', { groups: 'sales' }],
      ['another issuer', { iss: 'https://somewhere.else' }],
      ['another audience', { aud: 'not-us' }],
      ['an expiry in the past', { exp: Math.floor(Date.now() / 1000) - 3600 }],
    ] as [string, Record<string, unknown>][]) {
      const res = await this.server._fetch('/hello', {
        host: JWT_HOST,
        headers: { authorization: `Bearer ${this._token(extra)}` },
      })
      assert.equal(res.status, 401, why)
      assert.equal(res.headers['www-authenticate'], 'Bearer', why)
    }
  }

  async aTokenSignedBySomebodyElseIsRefused() {
    const head = Buffer.from(JSON.stringify({ alg: 'HS256', typ: 'JWT' })).toString('base64url')
    const body = Buffer.from(JSON.stringify({ sub: 'u-1', exp: this._exp })).toString('base64url')
    const forged = `${head}.${body}.${createHmac('sha256', 'not-the-secret').update(`${head}.${body}`).digest('base64url')}`
    const res = await this.server._fetch('/hello', {
      host: JWT_HOST,
      headers: { authorization: `Bearer ${forged}` },
    })
    assert.equal(res.status, 401)
  }
}

const FORWARD_HOST = 'forward.e2e.local'

/**
 * The operator's own endpoint, standing in for whatever they actually run.
 * `x-e2e-user: alice` is admitted and comes back as an identity; anything
 * else is sent to a login of the endpoint's own choosing.
 */
export class AuthCheckEndpoint extends Test({ timeout: 30_000 }) {
  _port = 0
  _server?: ReturnType<typeof createServer>

  async hookListen() {
    this._server = createServer((req, res) => {
      if (req.headers['x-e2e-user'] === 'alice') {
        res.writeHead(200, { 'x-auth-user': 'alice', 'x-not-asked-for': 'surprise' })
        res.end()
        return
      }
      res.writeHead(302, { location: 'https://sso.e2e.local/start' })
      res.end()
    })
    await new Promise<void>((ok) => this._server!.listen(0, '127.0.0.1', ok))
    this._port = (this._server!.address() as { port: number }).port
  }

  async cleanUp() {
    this._server?.close()
  }
}

export class ForwardServer extends AperioServerBase({
  env: { APERIO_VISITOR_IDENTITY_HEADERS: '1' },
  dependencies: { endpoint: () => AuthCheckEndpoint },
}) {
  declare endpoint: AuthCheckEndpoint
  _configFile() {
    return [
      'server:',
      '  auth:',
      '    - method: forward',
      `      url: http://127.0.0.1:${this.endpoint._port}/_authcheck`,
      '      request_headers: [x-e2e-user]',
      '      response_headers: [x-auth-user]',
      '',
    ].join('\n')
  }
}

export class ForwardBackend extends StandardBackendBase() {}

export class ForwardClient extends ClientFor(() => ForwardServer, () => ForwardBackend) {
  _hostname() {
    return ''
  }
  _env() {
    return { APERIO_HOSTNAME: FORWARD_HOST }
  }
}

export class ForwardMethodSpec extends Test({
  timeout: 90_000,
  dependencies: {
    endpoint: () => AuthCheckEndpoint,
    server: () => ForwardServer,
    backend: () => ForwardBackend,
    client: () => ForwardClient,
  },
}) {
  async theEndpointAdmitsAndItsHeaderReachesTheBackend() {
    await waitFor(
      async () => {
        const res = await this.server._fetch('/echo-headers', {
          host: FORWARD_HOST,
          headers: { 'x-e2e-user': 'alice' },
        })
        return res.status === 200
      },
      { label: 'the forward gate to admit' },
    )
    const res = await this.server._fetch('/echo-headers', {
      host: FORWARD_HOST,
      headers: { 'x-e2e-user': 'alice' },
    })
    // Named in `response_headers:`, so it crosses.
    assert.match(res.body, /x-auth-user: alice/)
    // Not named, so it does not: an open list is a header injection.
    assert.doesNotMatch(res.body, /x-not-asked-for/)
    assert.match(res.body, /x-aperio-visitor-how: forward/)
  }

  async aRefusalIsTheEndpointsOwnAnswer() {
    // Its 302, relayed rather than followed and rather than flattened into a
    // generic 401: sending the visitor to that login is the whole point.
    const res = await this.server._fetch('/hello', {
      host: FORWARD_HOST,
      headers: { 'x-e2e-user': 'mallory' },
    })
    assert.equal(res.status, 302)
    assert.equal(res.headers['location'], 'https://sso.e2e.local/start')
  }
}

const DECLARED_SECRET = '0123456789abcdef-client-declared'
const DECLARED_HOST = 'declared-gate.e2e.local'

/** Its own server: the polling below would otherwise drain the shared one's
 *  per-IP bucket and fail whatever test ran next. */
export class DeclaredGateServer extends AperioServerBase({
  env: { APERIO_SERVER_AUTH: 'demo:secret123' },
}) {}

export class DeclaredGateBackend extends StandardBackendBase() {}

class DeclaredGateClient extends ClientFor(
  () => DeclaredGateServer,
  () => DeclaredGateBackend,
) {}

/** A client whose own `auth:` is a method the scalar cannot carry. */
export class RichGateClient extends DeclaredGateClient {
  _autoStart() {
    return false
  }
  _hostname() {
    return ''
  }
  _config() {
    return [
      'server:',
      `  url: ${this.server._url}`,
      `  token: ${this.server._token}`,
      'services:',
      '  - name: declared_bearer',
      `    target: ${this._backendUrl()}`,
      `    hostname: ${DECLARED_HOST}`,
      '    auth:',
      '      method: bearer',
      `      secret: "${DECLARED_SECRET}"`,
      '',
    ].join('\n')
  }
}

/** A client declaring a method a client has no business declaring. */
export class ForwardDeclaringClient extends DeclaredGateClient {
  _autoStart() {
    return false
  }
  _hostname() {
    return ''
  }
  _config() {
    return [
      'server:',
      `  url: ${this.server._url}`,
      `  token: ${this.server._token}`,
      'services:',
      '  - name: declared_forward',
      `    target: ${this._backendUrl()}`,
      '    hostname: declared-forward.e2e.local',
      '    auth:',
      '      method: forward',
      '      url: http://127.0.0.1:9/_authcheck',
      '',
    ].join('\n')
  }
}

export class ClientDeclaredGateSpec extends Test({
  timeout: 90_000,
  dependencies: {
    server: () => DeclaredGateServer,
    backend: () => DeclaredGateBackend,
    rich: () => RichGateClient,
    forwarding: () => ForwardDeclaringClient,
  },
}) {
  async before() {
    await this.rich._start()
  }

  async aClientCanDeclareAGateTheScalarCannotCarry() {
    // The whole point: `bearer` travels now, because the server said on the
    // handshake that it understands it.
    await waitFor(
      async () => {
        const res = await this.server._fetch('/hello', {
          host: DECLARED_HOST,
          headers: { authorization: `Bearer ${DECLARED_SECRET}` },
        })
        return res.body === `backend ${this.backend._port} GET /hello`
      },
      { label: "the client's own bearer gate to admit" },
    )
  }

  async andItGatesEverythingElse() {
    const res = await this.server._fetch('/hello', { host: DECLARED_HOST })
    assert.equal(res.status, 401)
    assert.equal(res.headers['www-authenticate'], 'Bearer')
    // The server's own password does not open a client-declared gate, which
    // is the rule the scalar spelling already followed.
    const wrong = await this.server._fetch('/hello', {
      host: DECLARED_HOST,
      headers: { authorization: 'Bearer not-the-secret-here-at-all' },
    })
    assert.equal(wrong.status, 401)
  }

  async aMethodAClientMayNotDeclareIsRefusedByTheServer() {
    // `forward` would have the *server* call the URL, from the server's
    // network, so a client writing localhost would mean the server's. The
    // server does not announce it, so the client will not send it.
    await this.forwarding._start().catch(() => {})
    await this.forwarding._waitForLog('does not accept `forward`')
  }
}

/** The visitor password is a key to the site, not to Aperio. */
export class VisitorPlaneSpec extends Test({
  timeout: 90_000,
  after: () => [VisitorGateSpec],
  dependencies: {
    server: () => AuthServer,
    backend: () => AuthBackend,
    gated: () => GatedClient,
  },
}) {
  async theVisitorPasswordOpensTheSiteAndNotTheDashboard() {
    const login = await this.server._fetch(`/aperio/auth?redirect=/hello`, {
      host: HOST,
      method: 'POST',
      headers: {
        authorization: `Basic ${Buffer.from('demo:secret123').toString('base64')}`,
      },
    })
    assert.ok(login.status < 400, `the visitor password should log in, got ${login.status}`)
    const cookie = (login.headers['set-cookie'] ?? '').split(';')[0]
    assert.match(cookie, /aperio_session=/)

    // The site: served.
    await waitFor(
      async () => {
        const res = await this.server._fetch('/hello', { host: HOST, headers: { cookie } })
        return res.body === `backend ${this.backend._port} GET /hello`
      },
      { label: 'the visitor session to serve the site' },
    )

    // Aperio: not. The admin surface answers an unauthenticated caller by
    // sending them to the login page, so being sent there is the evidence
    // that this session authorized nothing administrative.
    const api = await this.server._fetch('/aperio/api/stats', { headers: { cookie } })
    assert.notEqual(api.status, 200, 'the visitor password reached the admin API')
    assert.match(api.headers['location'] ?? '', /\/aperio\/auth/)
  }
}
