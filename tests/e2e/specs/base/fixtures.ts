import { AperioServerBase } from '../../lib/server.js'
import { StandardBackendBase } from '../../lib/backend.js'
import { ClientFor, type ServerLike } from '../../lib/client.js'
import { join } from 'node:path'

export const HOST = 'app.e2e.local'
export const METRICS_TOKEN = 'e2e-metrics-token'
export const EDGE_TOKEN = 'e2e-edge-token'

/**
 * One configuration, a fresh class per spec file.
 *
 * Factories rather than classes, on purpose. Nole keys a dependency instance
 * by class identity, so `extends BaseServerFor()` in two files is two
 * servers, on two ports, with two data directories: the files share a *shape*
 * and nothing else, and can therefore run at the same time.
 *
 * They used to share one instance, and the phase was chained end to end with
 * `after:` to keep the specs off each other. That is serialization dressed as
 * ordering: it hides the coupling instead of removing it, it makes a single
 * file impossible to run on its own, and it leaves the phase as slow as its
 * whole line. The coupling was real, these specs put the server into
 * maintenance, edit its settings, count its connected clients and purge its
 * history, so the answer is to stop sharing the server, not to take turns
 * with it.
 *
 * `after:` still appears *within* a file, where it says something true: a
 * delivery cannot be read before the webhook that produced it was created.
 */
export function BaseServerFor() {
  return class extends AperioServerBase() {
    // Overridden rather than passed as `{ env }`: the access log lives inside
    // the data directory this instance is handed at startup.
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
}

export function BaseBackendFor() {
  return class extends StandardBackendBase() {}
}

/** The one client every file starts: a service on [`HOST`], ready at `/hello`. */
export function BaseClientFor<
  S extends new () => ServerLike,
  B extends new () => object,
>(server: () => S, backend: () => B) {
  return class extends ClientFor(server, backend) {
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
}
