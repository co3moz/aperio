import { AperioServerBase } from '../../lib/server.js'
import { StandardBackendBase } from '../../lib/backend.js'
import { ClientFor } from '../../lib/client.js'
import { join } from 'node:path'

export const HOST = 'app.e2e.local'
export const METRICS_TOKEN = 'e2e-metrics-token'
export const EDGE_TOKEN = 'e2e-edge-token'

/**
 * The base configuration, shared by every spec in this phase.
 *
 * One class rather than one file: in bash this phase is 1021 lines in a
 * single script because a second server would have meant a second phase, and
 * a phase is a file. Here the specs below are separate classes that name the
 * same server, so they read as separate subjects and still cost one process.
 */
export class BaseServer extends AperioServerBase() {
  _env() {
    return {
      APERIO_ACCESS_LOG: join(this._dataDir, 'access.jsonl'),
      APERIO_METRICS: '1',
      APERIO_METRICS_TOKEN: METRICS_TOKEN,
      APERIO_EDGE_TOKEN: EDGE_TOKEN,
      APERIO_EDGE_SERVICE_URL: 'http://aperio:8080',
      APERIO_EDGE_ENTRYPOINTS: 'websecure',
      APERIO_EDGE_CERT_RESOLVER: 'letsencrypt',
    }
  }

  _accessLog(): string {
    return join(this._dataDir, 'access.jsonl')
  }
}

export class BaseBackend extends StandardBackendBase() {}

export class BaseClient extends ClientFor(() => BaseServer, () => BaseBackend) {
  _hostname() {
    return HOST
  }
  _readyPath() {
    return '/hello'
  }
  _env() {
    return { APERIO_HOSTNAME: HOST }
  }
}
