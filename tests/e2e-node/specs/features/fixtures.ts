import { AperioServerBase } from '../../lib/server.js'
import { MockBackendBase, StandardBackendBase, type Route } from '../../lib/backend.js'
import { AperioClientBase, ClientFor } from '../../lib/client.js'
import { UnixBackendBase } from '../../lib/uds.js'

/** The features phase accumulates many concurrent clients, so the default
 *  ten-tunnel cap is lifted the way the bash phase lifts it. */
export class FeatureServer extends AperioServerBase({ env: { APERIO_MAX_TUNNELS: '30' } }) {}

export class MainBackend extends StandardBackendBase() {}
export class SecondBackend extends StandardBackendBase() {}
export class UdsBackend extends UnixBackendBase() {}

/** `/r` redirects within the same host, `/ext` to somewhere else entirely. */
export class RedirectBackend extends MockBackendBase({
  dependencies: { main: () => MainBackend },
}) {
  _routes(): Record<string, Route> {
    return {
      '/r': () => ({
        status: 302,
        headers: { location: '/hello' },
        body: '',
      }),
      '/ext': () => ({
        status: 301,
        headers: { location: 'https://example.com/elsewhere' },
        body: '',
      }),
      '*': (_req, url) => ({ body: `backend ${this._port} GET ${url.pathname}` }),
    }
  }
}

export class FeatureClient extends ClientFor(() => FeatureServer, () => MainBackend) {}

/** Started from a positional target rather than any setting. */
export class PositionalClient extends FeatureClient {
  _hostname() {
    return 'cli.e2e.local'
  }
  _readyPath() {
    return '/hello'
  }
  _args() {
    return [
      this.backend._url.replace('http://', ''),
      '--server-url',
      this.server._url,
      '--server-token',
      this.server._token,
      '--hostname',
      'cli.e2e.local',
    ]
  }
}

export class RedirectClient extends ClientFor(() => FeatureServer, () => RedirectBackend) {
  _hostname() {
    return 'redir.e2e.local'
  }
  _readyPath() {
    return '/r'
  }
  _env() {
    return { APERIO_HOSTNAME: 'redir.e2e.local' }
  }
}

/** Three services from one config file, each with its own settings. */
export class MultiServiceClient extends AperioClientBase({
  dependencies: {
    server: () => FeatureServer,
    web: () => MainBackend,
    api: () => SecondBackend,
  },
}) {
  _serverUrl() {
    return this.server._url
  }
  _serverToken() {
    return this.server._token
  }
  _hostname() {
    return 'web.e2e.local'
  }
  _readyPath() {
    return '/hello'
  }
  _config() {
    return [
      'server:',
      `  url: ${this.server._url}`,
      `  token: ${this.server._token}`,
      'services:',
      '  - name: web',
      `    target: ${this.web._url}`,
      '    hostname: web.e2e.local',
      '  - name: api',
      `    target: ${this.api._url}`,
      '    hostname: api.e2e.local',
      '    webhook_inbox: true',
      '  - name: upload',
      `    target: ${this.web._url}`,
      '    hostname: upload.e2e.local',
      '    max_request_body: 64',
      '    security_headers: true',
      '',
    ].join('\n')
  }
}

export class UnixSocketClient extends ClientFor(() => FeatureServer, () => UdsBackend) {
  _hostname() {
    return 'uds.e2e.local'
  }
  _readyPath() {
    return '/uds-hello'
  }
  _env() {
    return { APERIO_TARGET: this.backend._target(), APERIO_HOSTNAME: 'uds.e2e.local' }
  }
}

/** Reads its server and its health block from a `~/.aperio.yaml`. */
export class HomeConfigClient extends FeatureClient {
  _home = ''

  _autoStart() {
    return false
  }
  _serverUrl() {
    return this.server._url
  }
  _env() {
    return {
      HOME: this._home,
      USERPROFILE: this._home,
      APERIO_TARGET: this.backend._url,
      APERIO_HOSTNAME: 'home.e2e.local',
      // Deliberately unset, so what reaches the server proves the home file
      // was read.
      APERIO_SERVER_URL: '',
      APERIO_SERVER_TOKEN: '',
    }
  }
}

/** Uses a token whose secret the test mints. */
export class TokenScopedClient extends FeatureClient {
  _token = ''
  _extra: Record<string, string> = {}

  _autoStart() {
    return false
  }
  _serverToken() {
    return this._token
  }
  _env() {
    return this._extra
  }
}

export class AllowlistClient extends FeatureClient {
  _extra: Record<string, string> = {}
  _autoStart() {
    return false
  }
  _env() {
    return this._extra
  }
}

export class DenyRedirectClient extends AllowlistClient {}
export class UnrestrictedClient extends AllowlistClient {}
export class LoopbackAllowedClient extends AllowlistClient {}
