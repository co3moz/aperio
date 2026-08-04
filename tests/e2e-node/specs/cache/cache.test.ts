import { Test } from 'nole'
import assert from 'node:assert/strict'
import { sleep } from '../../lib/env.js'
import {
  CacheServer,
  CacheBackend,
  ResilientClient,
  PlainClient,
  SingleFlightBackend,
  SingleFlightClient,
  SwrBackend,
  SwrClient,
} from './fixtures.js'

const CACHE = 'cache.e2e.local'
const PLAIN = 'plain.e2e.local'

/**
 * The cache, and what a cached entry is worth when the client that filled it
 * goes away.
 *
 * One class, because these run in order and each leans on the one before:
 * the entry has to be warm before it can be revalidated, and warm before its
 * client can be killed out from under it.
 */
export class CacheSpec extends Test({
  timeout: 60_000,
  dependencies: {
    server: () => CacheServer,
    backend: () => CacheBackend,
    resilient: () => ResilientClient,
    plain: () => PlainClient,
  },
}) {
  async aSecondGetIsServedFromTheCache() {
    await this.server._fetch('/data', { host: CACHE })
    const res = await this.server._fetch('/data', { host: CACHE })
    assert.equal(res.headers['x-aperio-cache'], 'hit')
  }

  async theCachedEntryCarriesAValidator() {
    const warm = await this.server._fetch('/data', { host: CACHE })
    const etag = warm.headers['etag']
    assert.ok(etag, 'a cached entry is synthesized a validator when the backend sends none')

    const matched = await this.server._fetch('/data', {
      host: CACHE,
      headers: { 'if-none-match': etag },
    })
    assert.equal(matched.status, 304, 'a matching If-None-Match is answered edge-side')

    const other = await this.server._fetch('/data', {
      host: CACHE,
      headers: { 'if-none-match': '"other"' },
    })
    assert.equal(other.status, 200, 'a non-matching one still gets the body')
  }

  async aRouteWithoutResilienceFailsClosedWhileItsClientIsGone() {
    await this.server._fetch('/data', { host: PLAIN })
    await this.plain._kill()
    await sleep(1_000)
    const res = await this.server._fetch('/data', { host: PLAIN })
    assert.equal(res.status, 504, 'a cached entry is not a licence to answer for a dead service')
  }

  async aResilientRouteServesTheExpiredEntryAndSaysSo() {
    await this.resilient._kill()
    await sleep(2_000) // past max-age=1
    const res = await this.server._fetch('/data', { host: CACHE })
    assert.equal(res.status, 200)
    assert.equal(res.headers['x-aperio-stale'], 'true')
    assert.equal(res.body, 'cacheable /data', 'the stale body is the cached response')
  }

  async aReconnectedClientServesFreshAgain() {
    await this.resilient._start()
    const res = await this.server._fetch('/fresh-after-reconnect', { host: CACHE })
    assert.equal(res.headers['x-aperio-stale'], undefined, 'a live client is not answered stale')
  }
}

/** Concurrent identical misses must not each reach the backend. */
export class SingleFlightSpec extends Test({
  timeout: 60_000,
  dependencies: {
    server: () => CacheServer,
    backend: () => SingleFlightBackend,
    client: () => SingleFlightClient,
  },
}) {
  async fiveConcurrentMissesCollapseIntoOneUpstreamFetch() {
    await Promise.all(
      Array.from({ length: 5 }, () =>
        this.server._fetch('/coalesce-me', { host: 'sf.e2e.local' }),
      ),
    )
    assert.equal(this.backend._hitsFor('/coalesce-me'), 1)
  }

  async theFollowersLeaveAWarmCacheBehind() {
    const res = await this.server._fetch('/coalesce-me', { host: 'sf.e2e.local' })
    assert.equal(res.headers['x-aperio-cache'], 'hit')
  }
}

/** stale-while-revalidate, and the two things that read from a cached body. */
export class StaleWhileRevalidateSpec extends Test({
  timeout: 60_000,
  dependencies: {
    server: () => CacheServer,
    backend: () => SwrBackend,
    client: () => SwrClient,
  },
}) {
  async theExpiredEntryIsAnsweredAtOnceAndMarkedStale() {
    await this.server._fetch('/page', { host: 'swr.e2e.local' })
    await sleep(2_000) // past max-age=1, inside the 60s stale window
    const res = await this.server._fetch('/page', { host: 'swr.e2e.local' })
    assert.equal(res.headers['x-aperio-cache'], 'hit')
    assert.equal(res.headers['x-aperio-stale'], 'true')
    assert.equal(res.body, 'swr v1', 'the stale body is what was cached')
  }

  async theBackgroundRevalidationRefreshesTheEntry() {
    for (let i = 0; i < 100; i++) {
      const res = await this.server._fetch('/page', { host: 'swr.e2e.local' })
      if (res.body === 'swr v2') {
        assert.equal(res.headers['x-aperio-stale'], undefined, 'the refreshed entry serves fresh')
        return
      }
      await sleep(100)
    }
    assert.fail('the background revalidation never refreshed the entry')
  }

  async aRangedGetOnACachedEntryIsAnsweredFromTheCache() {
    await this.server._fetch('/rangefile', { host: 'swr.e2e.local' })
    const res = await this.server._fetch('/rangefile', {
      host: 'swr.e2e.local',
      headers: { range: 'bytes=0-3' },
    })
    assert.equal(res.status, 206)
    assert.equal(res.headers['x-aperio-cache'], 'hit')
    assert.match(res.headers['content-range'] ?? '', /^bytes 0-3\//)

    const beyond = await this.server._fetch('/rangefile', {
      host: 'swr.e2e.local',
      headers: { range: 'bytes=9999-' },
    })
    assert.equal(beyond.status, 416)
  }

  async aPurgedEntryIsFetchedAgain() {
    const cookie = await this.server._login()
    const purged = await this.server._json<{ status: string }>('/aperio/api/cache/purge', {
      method: 'POST',
      headers: { cookie, 'content-type': 'application/json' },
      body: JSON.stringify({ hostname: 'swr.e2e.local' }),
    })
    assert.equal(purged.status, 'ok')

    const res = await this.server._fetch('/page', { host: 'swr.e2e.local' })
    assert.notEqual(res.headers['x-aperio-cache'], 'hit')
  }
}
