import { Test } from 'nole'
import assert from 'node:assert/strict'
import { AperioServerBase } from '../../lib/server.js'
import { ClientFor } from '../../lib/client.js'
import { WsEchoBase, wsProbe } from '../../lib/ws.js'

export class WsServer extends AperioServerBase() {}
export class GreetingServer extends AperioServerBase() {}

export class EchoWsBackend extends WsEchoBase() {}

/** Speaks first, right after the 101. */
export class GreetingWsBackend extends WsEchoBase() {
  _greeting() {
    return 'greeting-first'
  }
}

export class EchoWsClient extends ClientFor(() => WsServer, () => EchoWsBackend) {
  _hostname() {
    return 'ws.e2e.local'
  }
  _readyPath() {
    return '/ping'
  }
  _env() {
    return { APERIO_HOSTNAME: 'ws.e2e.local' }
  }
}

export class GreetingWsClient extends ClientFor(() => GreetingServer, () => GreetingWsBackend) {
  _hostname() {
    return 'wsgreet.e2e.local'
  }
  _readyPath() {
    return '/ping'
  }
  _env() {
    return { APERIO_HOSTNAME: 'wsgreet.e2e.local' }
  }
}

export class WebSocketPassThroughSpec extends Test({
  timeout: 60_000,
  dependencies: {
    server: () => WsServer,
    backend: () => EchoWsBackend,
    client: () => EchoWsClient,
  },
}) {
  async aFrameIsEchoedThroughTheTunnel() {
    const echoed = await wsProbe(this.server._url, 'ws.e2e.local', 'hello-ws')
    assert.equal(echoed, 'echo:hello-ws')
  }
}

export class WebSocketGreetingSpec extends Test({
  timeout: 60_000,
  dependencies: {
    server: () => GreetingServer,
    backend: () => GreetingWsBackend,
    client: () => GreetingWsClient,
  },
}) {
  async aBackendThatSpeaksFirstReachesTheVisitor() {
    // Nothing is sent: the frame can only arrive if the greeting the backend
    // emitted right after its 101 survived the window in which the visitor's
    // own handshake was still completing.
    const greeting = await wsProbe(this.server._url, 'wsgreet.e2e.local', null)
    assert.equal(greeting, 'greeting-first')
  }
}
