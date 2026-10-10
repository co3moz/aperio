import { Test } from 'nole'
import assert from 'node:assert/strict'
import { AperioServerBase } from '../../lib/server.js'
import { StandardBackendBase } from '../../lib/backend.js'
import { ClientFor } from '../../lib/client.js'
import { waitFor } from '../../lib/env.js'
import { AuthCheckEndpoint } from './auth.test.js'

/** This file's own endpoint, so closing or reusing it touches no other spec. */
class ServerViaClientEndpoint extends AuthCheckEndpoint {}

/**
 * A `forward` with `via: client` that the *server* wrote: the URL is the
 * server's choice and the client is the one that calls it, from its own
 * network. A client answers that only when it agreed to with
 * `allow_server_forward`; otherwise a server could have every connected
 * client send requests wherever it liked. Unanswered, the gate fails closed.
 */
const HOST = 'server-via-client.e2e.local'

export class ServerViaClientServer extends AperioServerBase({
  env: { APERIO_VISITOR_IDENTITY_HEADERS: '1' },
  dependencies: { endpoint: () => ServerViaClientEndpoint },
}) {
  declare endpoint: ServerViaClientEndpoint
  _configFile() {
    return [
      'server:',
      '  auth:',
      '    - method: forward',
      `      url: http://127.0.0.1:${this.endpoint._port}/_authcheck`,
      '      via: client',
      '      request_headers: [x-e2e-user]',
      '      response_headers: [x-auth-user]',
      '      cache: 0',
      '',
    ].join('\n')
  }
}

export class ServerViaClientBackend extends StandardBackendBase() {}

export class ServerViaClientClient extends ClientFor(
  () => ServerViaClientServer,
  () => ServerViaClientBackend,
) {
  /** Set by the spec before a restart: the client's consent. */
  _optIn = false
  _autoStart() {
    return false
  }
  /** Gated by the server, which is the point; and not waited on as routable,
   *  since the first phase expects every request to be refused. */
  _public() {
    return false
  }
  _hostname() {
    return null
  }
  _env() {
    return {
      APERIO_HOSTNAME: HOST,
      ...(this._optIn ? { APERIO_ALLOW_SERVER_FORWARD: '1' } : {}),
    }
  }
}

export class ServerForwardViaClientSpec extends Test({
  timeout: 90_000,
  dependencies: {
    endpoint: () => ServerViaClientEndpoint,
    server: () => ServerViaClientServer,
    backend: () => ServerViaClientBackend,
    client: () => ServerViaClientClient,
  },
}) {
  async before() {
    await this.client._start()
    await this.server._waitForClients(1)
  }

  async aClientThatDidNotAgreeRefusesToCallTheServersEndpoint() {
    // Until the route is declared the answer is the closed posture's; once it
    // is, the server asks the client, the client refuses, and so does the gate.
    await waitFor(
      async () => {
        const res = await this.server._fetch('/hello', {
          host: HOST,
          headers: { 'x-e2e-user': 'alice' },
        })
        return res.status === 403
      },
      { label: 'the unanswered check to refuse the visitor' },
    )
    await this.client._waitForLog('allow_server_forward')
  }

  async withTheClientsConsentTheServersEndpointDecides() {
    await this.client._kill()
    await this.server._waitForClients(0)
    this.client._optIn = true
    await this.client._start()
    await this.server._waitForClients(1)
    await waitFor(
      async () => {
        const res = await this.server._fetch('/echo-headers', {
          host: HOST,
          headers: { 'x-e2e-user': 'alice' },
        })
        return res.status === 200
      },
      { label: 'the check the client agreed to answer to admit' },
    )
    const res = await this.server._fetch('/echo-headers', {
      host: HOST,
      headers: { 'x-e2e-user': 'alice' },
    })
    assert.match(res.body, /x-auth-user: alice/)
    // And the endpoint's own refusal is still what a stranger gets.
    const stranger = await this.server._fetch('/hello', {
      host: HOST,
      headers: { 'x-e2e-user': 'mallory' },
    })
    assert.equal(stranger.status, 302)
  }
}
