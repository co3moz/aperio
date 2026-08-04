import { randomUUID } from 'node:crypto'
import { AperioServerBase } from '../../lib/server.js'
import { MockBackendBase } from '../../lib/backend.js'
import { AperioClientBase } from '../../lib/client.js'
import { TcpEchoBase } from '../../lib/tcp.js'

/** Nothing special: tunnels need no server flags. */
export class TunnelServer extends AperioServerBase() {}

/** The HTTP side of the declaring client, so `hostname` is routable. */
export class TunnelBackend extends MockBackendBase() {
  _routes() {
    return {
      '*': (_req: unknown, url: URL) => ({ body: `backend ${url.pathname}` }),
    }
  }
}

export class EchoBackend extends TcpEchoBase() {}

/**
 * The client that *declares* the tunnels. Its id is fixed here because every
 * later assertion asks the server about it by name.
 */
export class DeclaringClient extends AperioClientBase({
  dependencies: {
    server: () => TunnelServer,
    backend: () => TunnelBackend,
    echo: () => EchoBackend,
  },
}) {
  readonly _id = randomUUID()

  _serverUrl() {
    return this.server._url
  }
  _serverToken() {
    return this.server._token
  }
  _hostname() {
    return 'decl.e2e.local'
  }
  _config() {
    return [
      'server:',
      `  url: ${this.server._url}`,
      `  token: ${this.server._token}`,
      `client_id: ${this._id}`,
      `target: ${this.backend._url}`,
      'hostname: decl.e2e.local',
      `tcp_target: ${this.echo._address()}`,
      'tunnels:',
      '  - name: echo_main',
      `    target: ${this.echo._address()}`,
      '    protocol: tcp',
      '  - name: echo_both',
      `    target: ${this.echo._address()}`,
      '    protocol: tcp/udp',
      '',
    ].join('\n')
  }
}

/**
 * Binds the declared tunnel by client id, overriding the local port.
 *
 * Started by the spec rather than by a hook: it is pointed at the declaring
 * client, so it may only come up once that client's declarations are
 * discoverable, and that is something a test asserts rather than something a
 * dependency can express.
 */
export class PortOverrideBinder extends AperioClientBase({
  dependencies: {
    server: () => TunnelServer,
    echo: () => EchoBackend,
    declaring: () => DeclaringClient,
  },
}) {
  _localPort = 0

  _autoStart() {
    return false
  }
  _serverUrl() {
    return this.server._url
  }
  _serverToken() {
    return this.server._token
  }
  _args() {
    return ['--bind-tunnels', this.declaring._id, '--server-url', this.server._url]
  }
  _config() {
    return [
      'bind-tunnels:',
      `  '${this.declaring._id}':`,
      `    token: ${this.server._token}`,
      '    override:',
      `      '${this.echo._address()}': ${this._localPort}`,
      '',
    ].join('\n')
  }
}

/** Binds by tunnel name, with a token whose only power is `allow_bind`. */
export class NamedBinder extends AperioClientBase({
  dependencies: {
    server: () => TunnelServer,
    declaring: () => DeclaringClient,
  },
}) {
  _bindToken = ''
  _mainPort = 0
  _bothPort = 0

  _autoStart() {
    return false
  }
  _serverUrl() {
    return this.server._url
  }
  _serverToken() {
    return this._bindToken
  }
  _args() {
    return ['--bind-tunnels']
  }
  _config() {
    return [
      'server:',
      `  url: ${this.server._url}`,
      `  token: ${this._bindToken}`,
      'bind-tunnels:',
      `  echo_main: ${this._mainPort}`,
      `  echo_both: ${this._bothPort}`,
      '',
    ].join('\n')
  }
}

/** The legacy `aperio-client tcp <port>` bridge. */
export class LegacyBridge extends AperioClientBase({
  dependencies: { server: () => TunnelServer, declaring: () => DeclaringClient },
}) {
  _localPort = 0

  _autoStart() {
    return false
  }
  _serverUrl() {
    return this.server._url
  }
  _serverToken() {
    return this.server._token
  }
  _args() {
    return [
      'tcp',
      String(this._localPort),
      '--server-url',
      this.server._url,
      '--server-token',
      this.server._token,
    ]
  }
}
