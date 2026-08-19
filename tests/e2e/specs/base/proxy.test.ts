import { Test } from 'nole'
import assert from 'node:assert/strict'
import { BaseServerFor, BaseBackendFor, BaseClientFor, HOST } from './fixtures.js'
import { FOREIGN_SERVER } from '../../lib/env.js'

/** This file's own server: the specs below change it, so it is not
 *  shared with another file. See `fixtures.ts`. */
class ProxyServer extends BaseServerFor() {}
class ProxyBackend extends BaseBackendFor() {}
class ProxyClient extends BaseClientFor(() => ProxyServer, () => ProxyBackend) {}

/** What the server answers before anything is connected to it. */
export class BareServerSpec extends Test({
  timeout: 60_000,
  dependencies: { server: () => ProxyServer },
}) {
  async healthReportsWhatThisBuildSpeaks() {
    const health = await this.server._json<{
      status: string
      protocol: number
      ui_language: string
    }>('/aperio/health')
    assert.equal(health.status, 'healthy')
    // The exact number is this build's, so it is only asserted when the
    // server *is* this build. Against a released binary (`test:compat`) the
    // question is whether it reports one at all, since a differing protocol
    // version between two releases is the normal case, not a fault.
    if (FOREIGN_SERVER) {
      assert.ok(health.protocol >= 1, 'the server reports a tunnel protocol version')
    } else {
      assert.equal(health.protocol, 9, 'the tunnel protocol version this build speaks')
    }
    assert.ok(health.ui_language, 'the default UI language is reported')
  }

  async aFreshInstallSendsTheBareRootToTheDashboard() {
    const res = await this.server._fetch('/')
    assert.equal(res.status, 307)
    assert.match(res.headers['location'] ?? '', /\/aperio/)
  }

  async proxyingWithoutAClientIs504() {
    assert.equal((await this.server._fetch('/hello')).status, 504)
  }

  async theAdminNamespaceIsNeverProxied() {
    // Without the redirect, a trailing slash falls through to the proxy and a
    // visitor who typed it gets a 504 or, with a client connected, somebody
    // else's tunnelled site.
    const slash = await this.server._fetch('/aperio/')
    assert.equal(slash.status, 308)
    const withQuery = await this.server._fetch('/aperio/?tab=clients')
    assert.match(withQuery.headers['location'] ?? '', /\/aperio\?tab=clients/)

    // A path under /aperio/ that matches nothing is a mistake, not traffic.
    assert.equal((await this.server._fetch('/aperio/api/does-not-exist')).status, 404)
  }
}

export class ProxyingSpec extends Test({
  // Ordered: this file's specs share one server and change it, so they
  // take turns. Files do not, they have a server each.
  after: () => [BareServerSpec],
  timeout: 90_000,
  dependencies: {
    server: () => ProxyServer,
    backend: () => ProxyBackend,
    client: () => ProxyClient,
  },
}) {
  async aGetIsProxiedWithItsQueryString() {
    const res = await this.server._fetch('/hello?x=1', { host: HOST })
    assert.equal(res.body, `backend ${this.backend._port} GET /hello?x=1`)
  }

  async aPostBodyIsProxied() {
    const res = await this.server._fetch('/submit', {
      host: HOST,
      method: 'POST',
      headers: { 'content-type': 'text/plain' },
      body: 'payload-123',
    })
    assert.equal(res.body, `backend ${this.backend._port} POST /submit body=payload-123`)
  }

  async aBufferedResponseBodySurvivesTheBinaryFrame() {
    // Since v5 a buffered body travels as bytes in the same frame as its
    // envelope. A base64 string could carry anything; a length-prefixed frame
    // has to be read correctly to, so the proof is 512 bytes covering every
    // value coming back unchanged.
    const want = Buffer.from([...Array(256).keys(), ...Array(256).keys()])
    const res = await this.server._fetch('/binary', { host: HOST })
    assert.deepEqual(res.bytes, want)
  }

  async aBufferedRequestBodySurvivesTheBinaryFrame() {
    const want = Buffer.from([...Array(256).keys(), ...Array(256).keys()])
    const res = await this.server._fetch('/echo-body', {
      host: HOST,
      method: 'POST',
      headers: { 'content-type': 'application/octet-stream' },
      body: want,
    })
    assert.deepEqual(res.bytes, want)
  }

  async aLargeBodyStreamsBothWays() {
    const big = Buffer.alloc(600_000, 'A')
    const res = await this.server._fetch('/echo-body', {
      host: HOST,
      method: 'POST',
      headers: { 'content-type': 'application/octet-stream' },
      body: big,
    })
    assert.equal(res.bytes.length, big.length)
  }
}

export class MaintenanceSpec extends Test({
  // Ordered: this file's specs share one server and change it, so they
  // take turns. Files do not, they have a server each.
  after: () => [ProxyingSpec],
  timeout: 60_000,
  dependencies: {
    server: () => ProxyServer,
    backend: () => ProxyBackend,
    client: () => ProxyClient,
  },
}) {
  async _maintenance(enabled: boolean) {
    const res = await this.server._api<{ status?: string }>('/aperio/api/maintenance', {
      method: 'POST',
      body: JSON.stringify({ hostname: HOST, enabled }),
    })
    return res
  }

  async aFlaggedHostAnswers503AndThenRecovers() {
    await this._maintenance(true)
    const flagged = await this.server._fetch('/hello', { host: HOST })
    assert.equal(flagged.status, 503)
    assert.ok(flagged.headers['retry-after'], 'maintenance says when to come back')

    await this._maintenance(false)
    const back = await this.server._fetch('/hello', { host: HOST })
    assert.equal(back.status, 200)
  }
}
