import { Test } from 'nole'
import assert from 'node:assert/strict'
import { AperioServerBase } from '../../lib/server.js'
import { StandardBackendBase } from '../../lib/backend.js'
import { ClientFor } from '../../lib/client.js'
import { waitFor } from '../../lib/env.js'

const HOST = 'app.e2e.local'

export class BackendOne extends StandardBackendBase() {}
export class BackendTwo extends StandardBackendBase() {}

/**
 * Two servers, because a strategy is chosen at startup and the two strategies
 * are two different questions.
 *
 * In bash these were one phase that started a server, stopped it, and started
 * another on the same port, which is why the phase had to remember to call
 * `stop_server` in the middle. Here they are two classes and can even run at
 * the same time.
 */
export class PrimaryStandbyServer extends AperioServerBase() {
  _env() {
    return { APERIO_LB_STRATEGY: 'primary-standby' }
  }
}

export class StickyServer extends AperioServerBase() {
  _env() {
    return { APERIO_LB_STRATEGY: 'sticky' }
  }
}

export class PrimaryClient extends ClientFor(() => PrimaryStandbyServer, () => BackendOne) {
  _hostname() {
    return HOST
  }
  _env() {
    return { APERIO_HOSTNAME: HOST }
  }
}

export class StandbyClient extends ClientFor(() => PrimaryStandbyServer, () => BackendTwo) {
  _env() {
    return { APERIO_HOSTNAME: HOST, APERIO_PRIORITY: '1' }
  }
}

export class StickyClientA extends ClientFor(() => StickyServer, () => BackendOne) {
  _hostname() {
    return HOST
  }
  _env() {
    return { APERIO_HOSTNAME: HOST }
  }
}

export class StickyClientB extends ClientFor(() => StickyServer, () => BackendTwo) {
  _env() {
    return { APERIO_HOSTNAME: HOST }
  }
}

/** Which backend answered, from the body the standard backend writes. */
function servedBy(body: string): number {
  const port = /^backend (\d+) /.exec(body)?.[1]
  assert.ok(port, `could not tell which backend answered: ${body}`)
  return Number(port)
}

export class PrimaryStandbySpec extends Test({
  timeout: 90_000,
  dependencies: {
    server: () => PrimaryStandbyServer,
    primaryBackend: () => BackendOne,
    standbyBackend: () => BackendTwo,
    primary: () => PrimaryClient,
    standby: () => StandbyClient,
  },
}) {
  async _bothAnnounced() {
    await this.server._waitForClients(2)
    // The priority rides on the standby's first heartbeat. Until it lands
    // both clients look like tier 0 and the assertions below would be
    // measuring round-robin instead.
    await waitFor(
      async () => {
        const stats = await this.server._api<{ active_clients: { priority: number }[] }>(
          '/aperio/api/stats',
        )
        return stats.active_clients.some((c) => c.priority === 1)
      },
      { label: 'the standby to announce its priority' },
    )
  }

  async everyRequestGoesToThePrimary() {
    await this._bothAnnounced()
    for (let i = 0; i < 4; i++) {
      const res = await this.server._fetch('/tier', { host: HOST })
      assert.equal(servedBy(res.body), this.primaryBackend._port, `request ${i + 1}`)
    }
  }

  async theStandbyTakesOverWhenThePrimaryDies() {
    await this.primary._kill()
    await waitFor(
      async () => {
        const res = await this.server._fetch('/tier', { host: HOST })
        return res.status === 200 && servedBy(res.body) === this.standbyBackend._port
      },
      { label: 'the standby to take over' },
    )
  }
}

export class StickySpec extends Test({
  timeout: 90_000,
  dependencies: {
    server: () => StickyServer,
    a: () => StickyClientA,
    b: () => StickyClientB,
  },
}) {
  async aVisitorStaysOnTheBackendItLandedOn() {
    await this.server._waitForClients(2)
    const first = await this.server._fetch('/pin', { host: HOST })
    const cookie = first.headers['set-cookie'] ?? ''
    assert.match(cookie, /aperio_affinity/, 'the first answer pins the visitor')
    const pinned = servedBy(first.body)

    const affinity = cookie.split(';')[0]
    for (let i = 0; i < 5; i++) {
      const res = await this.server._fetch('/pin', { host: HOST, headers: { cookie: affinity } })
      assert.equal(servedBy(res.body), pinned, `follow-up ${i + 1} moved`)
    }
  }
}
