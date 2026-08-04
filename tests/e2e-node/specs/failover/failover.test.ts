import { Test } from 'nole'
import assert from 'node:assert/strict'
import { AperioServerBase } from '../../lib/server.js'
import { StandardBackendBase } from '../../lib/backend.js'
import { ClientFor } from '../../lib/client.js'
import { send } from '../../lib/http.js'
import { sleep } from '../../lib/env.js'

const HOST = 'app.e2e.local'

export class FailoverServer extends AperioServerBase({
  env: { APERIO_FAILOVER: 'retry-wait', APERIO_FAILOVER_WINDOW: '20' },
}) {}

export class FailoverBackend extends StandardBackendBase() {}

class FailoverClient extends ClientFor(() => FailoverServer, () => FailoverBackend) {
  _hostname() {
    return HOST
  }
  _env() {
    return { APERIO_HOSTNAME: HOST }
  }
}

export class FirstClient extends FailoverClient {}

/** The replacement. Started by the test, in the middle of a request. */
export class SecondClient extends FailoverClient {
  _autoStart() {
    return false
  }
}

/**
 * A request already in flight when its client dies has to land somewhere
 * else, not fail.
 */
export class FailoverSpec extends Test({
  timeout: 60_000,
  dependencies: {
    server: () => FailoverServer,
    backend: () => FailoverBackend,
    first: () => FirstClient,
    second: () => SecondClient,
  },
}) {
  async anInFlightRequestSurvivesItsClientBeingKilled() {
    // `/slow` sits in the backend for five seconds, which is the window the
    // kill has to land in.
    const inFlight = send(this.server._url, '/slow', { host: HOST })
    await sleep(1_000)
    await this.first._kill()
    await this.second._start()

    const res = await inFlight
    assert.equal(res.status, 200)
    assert.equal(res.body, `backend ${this.backend._port} GET /slow`)
  }

  async theServerSaysItJumped() {
    await this.server._waitForLog('In-flight failover')
  }
}
