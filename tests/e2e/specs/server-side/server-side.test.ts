import { Test } from 'nole'
import assert from 'node:assert/strict'
import { AperioServerBase } from '../../lib/server.js'
import { StandardBackendBase } from '../../lib/backend.js'
import { WsEchoBase, wsProbe } from '../../lib/ws.js'
import { AperioClientBase } from '../../lib/client.js'

/**
 * A server that will reach loopback on a client's behalf.
 *
 * The allowlist is the permission, so the fixture has to name something for
 * anything to be served this way at all. `127.0.0.1` is what the backends in
 * this phase listen on, and `10.0.0.0/8` is here to be the entry that does
 * *not* match the second client's target: a refusal has to be tested against a
 * configured list rather than an empty one, or it would only prove that an
 * unconfigured server refuses everything.
 */
export class ServerSideServer extends AperioServerBase({
  env: {
    APERIO_RANDOM_SUBDOMAIN: '',
    APERIO_SERVER_SIDE_TARGETS: '127.0.0.1,10.0.0.0/8',
  },
}) {}

export class DirectBackend extends StandardBackendBase() {}
export class DirectWsBackend extends WsEchoBase() {}

/**
 * One service the server reaches itself, and one WebSocket backend beside it.
 *
 * Both are ordinary targets on loopback. What makes this phase mean anything
 * is not that the requests succeed, a relayed service would succeed too, but
 * the pair of refusals further down: a target the allowlist does not name and
 * a token without the permission are both refused, and neither would be if the
 * declaration were being ignored and quietly relayed.
 */
export class DirectClient extends AperioClientBase({
  dependencies: {
    server: () => ServerSideServer,
    web: () => DirectBackend,
    ws: () => DirectWsBackend,
  },
}) {
  _serverUrl() {
    return this.server._url
  }
  _serverToken() {
    return this.server._token
  }
  _config() {
    return [
      'server:',
      `  url: ${this.server._url}`,
      `  token: ${this.server._token}`,
      'services:',
      '  - name: ss_web',
      `    target: ${this.web._url}`,
      '    hostname: ss-web.e2e.local',
      '    server_side: true',
      '  - name: ss_ws',
      `    target: ${this.ws._url}`,
      '    hostname: ss-ws.e2e.local',
      '    server_side: true',
      '',
    ].join('\n')
  }
}

export class ServerSideSpec extends Test({
  timeout: 120_000,
  dependencies: {
    server: () => ServerSideServer,
    web: () => DirectBackend,
    ws: () => DirectWsBackend,
    client: () => DirectClient,
  },
}) {
  /**
   * Waits for the declaration to be applied, not merely for the socket.
   *
   * A client is connected a moment before the server has read its heartbeat
   * and admitted what it declares, and all three tests below ask for a route
   * that exists only after that. Without this they failed together, in about
   * a millisecond, in roughly one run in four.
   */
  async hookDeclared() {
    await this.server._waitForLog('from this server')
  }

  async aRequestIsAnsweredByTheBackendTheServerReached() {
    const res = await this.server._fetch('/hello?x=1', { host: 'ss-web.e2e.local' })
    assert.equal(res.status, 200)
    assert.match(res.body, /GET \/hello\?x=1/, res.body)
  }

  /**
   * The strip that keeps a visitor's framing headers away from the backend.
   *
   * This is the one that shipped missing. The relayed path drops these in the
   * client, and the server-side path reached a backend the same way while
   * dropping none of them, which made serving from the server a way around a
   * defence rather than a second road to the same place. `/echo-headers`
   * reports what actually arrived, so this asserts the outcome rather than the
   * list.
   */
  async aVisitorsFramingHeadersDoNotReachTheBackend() {
    const res = await this.server._fetch('/echo-headers', {
      host: 'ss-web.e2e.local',
      headers: {
        'sec-websocket-version': '13',
        'sec-websocket-key': 'dGhlIHNhbXBsZSBub25jZQ==',
        'x-ordinary': 'travels',
      },
    })
    assert.equal(res.status, 200)
    const seen = res.body.toLowerCase()
    assert.ok(!seen.includes('sec-websocket-version'), `stripped, got:\n${res.body}`)
    assert.ok(!seen.includes('sec-websocket-key'), `stripped, got:\n${res.body}`)
    // And the strip is a named list rather than a filter that eats everything.
    assert.ok(seen.includes('x-ordinary: travels'), `ordinary headers travel:\n${res.body}`)
  }

  /**
   * The socket half, and the one assertion in this phase that proves *who*
   * dialed rather than that something answered.
   *
   * A relayed upgrade reaches the client and is logged as one. This line is
   * only written where the server opened the socket itself.
   */
  async aWebSocketIsSplicedToOneTheServerOpened() {
    const reply = await wsProbe(this.server._url, 'ss-ws.e2e.local', 'ping', { path: '/' })
    assert.match(reply, /ping/)
    await this.server._waitForLog('WebSocket served from this server')
  }
}

/** A client whose target the allowlist does not name. */
export class RefusedTargetClient extends AperioClientBase({
  dependencies: { server: () => ServerSideServer },
}) {
  /** Started by the test, so the log assertion cannot race the declaration. */
  _autoStart() {
    return false
  }
  _serverUrl() {
    return this.server._url
  }
  _serverToken() {
    return this.server._token
  }
  _config() {
    return [
      'server:',
      `  url: ${this.server._url}`,
      `  token: ${this.server._token}`,
      'services:',
      '  - name: ss_refused',
      // Syntactically fine, on no entry of the configured list, and nothing
      // here ever connects to it: the declaration is refused before it could.
      '    target: http://192.168.44.44:9000',
      '    hostname: ss-refused.e2e.local',
      '    server_side: true',
      '',
    ].join('\n')
  }
}

export class RefusedTargetSpec extends Test({
  timeout: 120_000,
  dependencies: {
    server: () => ServerSideServer,
    refused: () => RefusedTargetClient,
  },
}) {

  /**
   * A target outside the list is refused, and the route is not served.
   *
   * This is what makes the whole phase mean something. If `server_side:` were
   * accepted and then ignored, this service would be relayed and would answer
   * perfectly well, because its client is connected and could reach nothing
   * either way. It answers as an unclaimed hostname does instead, and the
   * server says which target and which setting.
   */
  async aTargetTheAllowlistDoesNotNameIsRefusedRatherThanRelayed() {
    await this.refused._start()
    await this.server._waitForLog('not on server_side_targets')

    const res = await this.server._fetch('/', { host: 'ss-refused.e2e.local' })
    assert.notEqual(res.status, 200)
  }
}

/** A token minted without the permission, everything else the same. */
export class UnpermittedClient extends AperioClientBase({
  dependencies: { server: () => ServerSideServer },
}) {
  /** Started by the test: its token does not exist until the test mints it. */
  _autoStart() {
    return false
  }
  _token = ''
  _serverUrl() {
    return this.server._url
  }
  _serverToken() {
    return this._token
  }
  _config() {
    return [
      'server:',
      `  url: ${this.server._url}`,
      `  token: ${this._token}`,
      'services:',
      '  - name: ss_unpermitted',
      `    target: ${this.server._url}`,
      '    hostname: ss-unpermitted.e2e.local',
      '    server_side: true',
      '',
    ].join('\n')
  }
}

export class UnpermittedSpec extends Test({
  timeout: 120_000,
  dependencies: {
    server: () => ServerSideServer,
    unpermitted: () => UnpermittedClient,
  },
}) {

  /**
   * The two permissions are separate, and this proves it from the outside.
   *
   * The target here is one the allowlist *does* name, so the only thing left
   * to refuse on is the token. A single combined permission would have let
   * this through.
   */
  async aTokenWithoutThePermissionIsRefusedEvenForAnAllowedTarget() {
    const minted = await this.server._mintToken({
      name: 'no-server-side',
      hostnames: ['ss-unpermitted.e2e.local'],
      allow_server_side: false,
    })
    this.unpermitted._token = minted.token
    await this.unpermitted._start()
    await this.server._waitForLog('allow_server_side')

    const res = await this.server._fetch('/', { host: 'ss-unpermitted.e2e.local' })
    assert.notEqual(res.status, 200)
  }
}
