import { Test } from 'nole'
import assert from 'node:assert/strict'
import { waitFor, sleep } from '../../lib/env.js'
import {
  FeatureServer,
  MainBackend,
  TokenScopedClient,
  AllowlistClient,
  DenyRedirectClient,
  UnrestrictedClient,
  LoopbackAllowedClient,
} from './fixtures.js'

export class RateLimitedClient extends TokenScopedClient {}
export class ConnectionCappedClient extends TokenScopedClient {}

/** A token's own rate limit, and the refusal saying which limit fired. */
export class TokenRateLimitSpec extends Test({
  timeout: 90_000,
  dependencies: {
    server: () => FeatureServer,
    backend: () => MainBackend,
    client: () => RateLimitedClient,
  },
}) {
  async before() {
    const minted = await this.server._mintToken({
      name: 'rl',
      hostnames: ['rl.e2e.local'],
      max_rps: 1,
    })
    this.client._token = minted.token
    this.client._extra = { APERIO_HOSTNAME: 'rl.e2e.local' }
    await this.client._start()
    await this.client._waitRoutable('rl.e2e.local', '/hello')
    // The routability probe spent the bucket's one token; let it refill so the
    // burst below sees both a pass and a refusal.
    await sleep(2_000)
  }

  async aBurstSeesBothPassesAndRefusals() {
    const codes: number[] = []
    for (let i = 0; i < 8; i++) {
      codes.push((await this.server._fetch('/limited', { host: 'rl.e2e.local' })).status)
    }
    assert.ok(codes.includes(200), `no request passed: ${codes}`)
    assert.ok(codes.includes(429), `nothing was refused: ${codes}`)
  }

  async theRefusalNamesTheLimitAndWhereItsNumberLives() {
    const res = await this.server._fetch('/limited', { host: 'rl.e2e.local' })
    assert.equal(res.status, 429)
    // Without this, finding the number to raise means reading the server's
    // log next to a load test.
    // One header carries both: `x-aperio-limit: token-rate; setting=…`.
    const limit = res.headers['x-aperio-limit'] ?? ''
    assert.match(limit, /^token-rate\b/, `got ${limit}`)
    assert.match(limit, /setting=token\.max_rps/, `got ${limit}`)
    assert.ok(res.headers['retry-after'], 'a limit that refills says when to come back')
  }
}

/** The server announces the ceiling; a token may only lower it. */
export class ConnectionCeilingSpec extends Test({
  timeout: 90_000,
  dependencies: {
    server: () => FeatureServer,
    backend: () => MainBackend,
    client: () => ConnectionCappedClient,
  },
}) {
  async aClientAskingForMoreOpensWhatItIsAllowed() {
    const minted = await this.server._mintToken({
      name: 'conns',
      hostnames: ['conns.e2e.local'],
      max_connections: 2,
    })
    this.client._token = minted.token
    this.client._extra = { APERIO_HOSTNAME: 'conns.e2e.local', APERIO_CONNECTIONS: '6' }
    await this.client._start()
    await this.client._waitRoutable('conns.e2e.local', '/hello')

    await waitFor(
      () => (this.client._log().match(/Successfully connected/g) ?? []).length === 2,
      { label: 'exactly two connections' },
    )
    // The rest say so instead of opening sockets the server would close.
    assert.match(this.client._log(), /stands down/)
  }
}

/**
 * The per-candidate visitor allowlist. Union semantics: an unrestricted
 * candidate on the same route admits the visitor, because route-wide lockdown
 * belongs to the token's own IP allowlist.
 */
export class VisitorAllowlistSpec extends Test({
  timeout: 120_000,
  dependencies: {
    server: () => FeatureServer,
    backend: () => MainBackend,
    denied: () => AllowlistClient,
    redirected: () => DenyRedirectClient,
    unrestricted: () => UnrestrictedClient,
    loopback: () => LoopbackAllowedClient,
  },
}) {
  async aFullyRejectedVisitorGetsTheStealthUnclaimedRouteAnswer() {
    this.denied._extra = {
      APERIO_HOSTNAME: 'ipdeny.e2e.local',
      APERIO_ALLOWED_IPS: '203.0.113.7',
    }
    await this.denied._start()
    await waitFor(
      async () => (await this.server._fetch('/hello', { host: 'ipdeny.e2e.local' })).status === 504,
      { label: 'the allowlist to start rejecting' },
    )
    // 504, not 403: a route-revealing answer would say the route exists.
  }

  async aRejectedVisitorIsSentToTheDeclaredDeniedPage() {
    this.redirected._extra = {
      APERIO_HOSTNAME: 'ipredir.e2e.local',
      APERIO_ALLOWED_IPS: '203.0.113.7',
      APERIO_DENIED: 'https://example.com/denied',
    }
    await this.redirected._start()
    await waitFor(
      async () =>
        (await this.server._fetch('/hello', { host: 'ipredir.e2e.local' })).status === 302,
      { label: 'the denied redirect to take effect' },
    )
    const res = await this.server._fetch('/hello', { host: 'ipredir.e2e.local' })
    assert.equal(res.headers['location'], 'https://example.com/denied')
  }

  async anUnrestrictedCandidateOnTheSameRouteAdmitsTheVisitor() {
    this.unrestricted._extra = { APERIO_HOSTNAME: 'ipdeny.e2e.local' }
    await this.unrestricted._start()
    await this.unrestricted._waitRoutable('ipdeny.e2e.local', '/hello')
    const res = await this.server._fetch('/hello', { host: 'ipdeny.e2e.local' })
    assert.match(res.body, new RegExp(`^backend ${this.backend._port} `))
  }

  async aVisitorInsideTheAllowedCidrIsServed() {
    this.loopback._extra = {
      APERIO_HOSTNAME: 'ipallow.e2e.local',
      APERIO_ALLOWED_IPS: '127.0.0.0/8',
    }
    await this.loopback._start()
    await this.loopback._waitRoutable('ipallow.e2e.local', '/hello')
    const res = await this.server._fetch('/hello', { host: 'ipallow.e2e.local' })
    assert.match(res.body, new RegExp(`^backend ${this.backend._port} `))
  }
}
