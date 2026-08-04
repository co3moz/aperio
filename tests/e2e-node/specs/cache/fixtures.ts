import { AperioServerBase } from '../../lib/server.js'
import { MockBackendBase } from '../../lib/backend.js'
import { ClientFor } from '../../lib/client.js'
import { sleep } from '../../lib/env.js'

/** The server every cache spec shares: one process, one port, named once. */
export class CacheServer extends AperioServerBase() {
  _env() {
    return { APERIO_CACHE: '1', APERIO_CACHE_MAX_STALE: '60' }
  }
}

/** Answers anything, and allows shared caching for a second. */
export class CacheBackend extends MockBackendBase() {
  _routes() {
    return {
      '*': (_req: unknown, url: URL) => ({
        body: `cacheable ${url.pathname}`,
        headers: { 'cache-control': 'max-age=1' },
      }),
    }
  }
}

/** Slow, and counts what reaches it: for coalescing. */
export class SingleFlightBackend extends MockBackendBase() {
  _routes() {
    return {
      '*': async (_req: unknown, url: URL) => {
        await sleep(1_000)
        return {
          body: `slow ${url.pathname}`,
          headers: { 'cache-control': 'max-age=60' },
        }
      },
    }
  }
}

/** A new body on every fetch, so a refresh is visible in what comes back. */
export class SwrBackend extends MockBackendBase() {
  _version = 0
  _routes() {
    return {
      '*': () => ({
        body: `swr v${++this._version}`,
        headers: { 'cache-control': 'max-age=1, stale-while-revalidate=60' },
      }),
    }
  }
}

class CacheClient extends ClientFor(() => CacheServer, () => CacheBackend) {}

/** Caching plus `resilience`: may serve a cached body while it is offline. */
export class ResilientClient extends CacheClient {
  _hostname() {
    return 'cache.e2e.local'
  }
  _env() {
    return {
      APERIO_HOSTNAME: 'cache.e2e.local',
      APERIO_CACHE: '1',
      APERIO_RESILIENCE: '1',
    }
  }
}

/** Caching without it: must fail closed while offline. */
export class PlainClient extends CacheClient {
  _hostname() {
    return 'plain.e2e.local'
  }
  _env() {
    return { APERIO_HOSTNAME: 'plain.e2e.local', APERIO_CACHE: '1' }
  }
}

export class SingleFlightClient extends ClientFor(() => CacheServer, () => SingleFlightBackend) {
  _hostname() {
    return 'sf.e2e.local'
  }
  _env() {
    return { APERIO_HOSTNAME: 'sf.e2e.local', APERIO_CACHE: '1' }
  }
}

export class SwrClient extends ClientFor(() => CacheServer, () => SwrBackend) {
  _hostname() {
    return 'swr.e2e.local'
  }
  _env() {
    return { APERIO_HOSTNAME: 'swr.e2e.local', APERIO_CACHE: '1' }
  }
}
