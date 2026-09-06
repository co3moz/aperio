import { Test } from 'nole'
import assert from 'node:assert/strict'
import { AperioServerBase } from '../../lib/server.js'
import { StandardBackendBase } from '../../lib/backend.js'
import { ClientFor } from '../../lib/client.js'
import { waitFor } from '../../lib/env.js'
import { AuthCheckEndpoint } from './auth.test.js'

/**
 * `forward` asked over the tunnel (planned_features #157): the endpoint that
 * decides is reachable from the client's network only, and a client declares
 * it with `via: client`. The server, holding a request it cannot yet admit,
 * asks the connection that would serve it; the verdict comes back the way a
 * response does.
 */
const HOST = 'via-client.e2e.local'

/** Its own server: the pre-auth asks spend from the visitor's IP bucket, and
 *  the shared one's would be drained for whatever test ran next. */
export class ViaClientServer extends AperioServerBase({
  env: { APERIO_VISITOR_IDENTITY_HEADERS: '1' },
}) {}

export class ViaClientBackend extends StandardBackendBase() {}

/** A client whose gate is its own endpoint, asked over the tunnel. The URL is
 *  the endpoint's loopback address, which only means anything on this side. */
export class ViaClientClient extends ClientFor(() => ViaClientServer, () => ViaClientBackend) {
  declare endpoint: AuthCheckEndpoint
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
      '  - name: own_gate',
      `    target: ${this._backendUrl()}`,
      `    hostname: ${HOST}`,
      '    auth:',
      '      method: forward',
      `      url: http://127.0.0.1:${this.endpoint._port}/_authcheck`,
      '      via: client',
      '      request_headers: [x-e2e-user]',
      '      response_headers: [x-auth-user]',
      '      cache: 0',
      '',
    ].join('\n')
  }
}

export class ForwardViaClientSpec extends Test({
  timeout: 90_000,
  dependencies: {
    endpoint: () => AuthCheckEndpoint,
    server: () => ViaClientServer,
    backend: () => ViaClientBackend,
    client: () => ViaClientClient,
  },
}) {
  async before() {
    this.client.endpoint = this.endpoint
    await this.client._start()
    await this.server._waitForClients(1)
  }

  async theClientsOwnEndpointAdmitsAndItsHeaderReachesTheBackend() {
    await waitFor(
      async () => {
        const res = await this.server._fetch('/echo-headers', {
          host: HOST,
          headers: { 'x-e2e-user': 'alice' },
        })
        return res.status === 200
      },
      { label: 'the gate asked over the tunnel to admit' },
    )
    const res = await this.server._fetch('/echo-headers', {
      host: HOST,
      headers: { 'x-e2e-user': 'alice' },
    })
    // Named in `response_headers:`, so it crosses, and the server enforces
    // the list on its side whatever the client sent back.
    assert.match(res.body, /x-auth-user: alice/)
    assert.doesNotMatch(res.body, /x-not-asked-for/)
    assert.match(res.body, /x-aperio-visitor-how: forward/)
  }

  async aRefusalIsTheEndpointsOwnAnswerRelayedOverTheTunnel() {
    const res = await this.server._fetch('/hello', {
      host: HOST,
      headers: { 'x-e2e-user': 'mallory' },
    })
    assert.equal(res.status, 302)
    assert.equal(res.headers['location'], 'https://sso.e2e.local/start')
  }

  async nobodyAnsweringRefusesRatherThanAdmits() {
    // The endpoint gone: the client cannot ask it, and the gate fails closed.
    this.endpoint._server?.close()
    await new Promise((ok) => setTimeout(ok, 200))
    const res = await this.server._fetch('/hello', {
      host: HOST,
      headers: { 'x-e2e-user': 'alice' },
    })
    assert.equal(res.status, 403)
  }
}
