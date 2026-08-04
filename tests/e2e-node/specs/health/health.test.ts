import { Test } from 'nole'
import assert from 'node:assert/strict'
import { AperioServerBase } from '../../lib/server.js'
import { StandardBackendBase } from '../../lib/backend.js'
import { ClientFor } from '../../lib/client.js'
import { waitFor, sleep } from '../../lib/env.js'

export class HealthServer extends AperioServerBase() {}
export class HealthBackend extends StandardBackendBase() {}
/** A port nothing listens on until the gate test starts it. */
export class GateBackend extends StandardBackendBase() {}

class HealthClient extends ClientFor(() => HealthServer, () => HealthBackend) {}

export class ProbedClient extends HealthClient {
  _hostname() {
    return 'health.e2e.local'
  }
  _readyPath() {
    return '/hello'
  }
  _env() {
    return {
      APERIO_HOSTNAME: 'health.e2e.local',
      APERIO_TARGET_HEALTH: '/health',
      APERIO_HEALTH_INTERVAL: '1',
      APERIO_HEALTH_TIMEOUT: '1',
      APERIO_HEALTH_THRESHOLD: '2',
    }
  }
}

/** A five-second interval, so being caught quickly proves the first probe
 *  fired at once rather than after one interval. */
export class DeadBackendClient extends HealthClient {
  _autoStart() {
    return false
  }
  _env() {
    return {
      APERIO_HOSTNAME: 'dead.e2e.local',
      APERIO_TARGET_HEALTH: '/health',
      APERIO_HEALTH_INTERVAL: '5',
      APERIO_HEALTH_TIMEOUT: '1',
      APERIO_HEALTH_THRESHOLD: '1',
    }
  }
}

/** A thirty-second interval: becoming routable at all proves it did not wait
 *  for one, and did not stay stuck unhealthy. */
export class SlowIntervalClient extends HealthClient {
  _autoStart() {
    return false
  }
  _env() {
    return {
      APERIO_HOSTNAME: 'slow.e2e.local',
      APERIO_TARGET_HEALTH: '/health',
      APERIO_HEALTH_INTERVAL: '30',
      APERIO_HEALTH_TIMEOUT: '1',
      APERIO_HEALTH_THRESHOLD: '1',
    }
  }
}

export class WaitForBackendClient extends ClientFor(() => HealthServer, () => GateBackend) {
  _autoStart() {
    return false
  }
  _env() {
    return { APERIO_HOSTNAME: 'waitgate.e2e.local', APERIO_WAIT_FOR_BACKEND: '1' }
  }
}

interface ClientView {
  backend_healthy: boolean
  backend_probed: boolean
  service?: string | null
}

export class BackendHealthSpec extends Test({
  timeout: 120_000,
  dependencies: {
    server: () => HealthServer,
    backend: () => HealthBackend,
    client: () => ProbedClient,
  },
}) {
  async _health(): Promise<ClientView | undefined> {
    const stats = await this.server._api<{ active_clients: ClientView[] }>('/aperio/api/stats')
    return stats.active_clients[0]
  }

  async aLiveBackendBecomesHealthyAndSaysItWasProbed() {
    await waitFor(async () => (await this._health())?.backend_healthy === true, {
      label: 'the backend to be reported healthy',
    })
    // The probed flag is what lets the dashboard show "checking" rather than
    // "down" before the first probe lands.
    assert.equal((await this._health())?.backend_probed, true)
  }

  async aDeadBackendIsReportedUnhealthyAndLeavesRouting() {
    await this.backend._stop()
    await waitFor(async () => (await this._health())?.backend_healthy === false, {
      timeoutMs: 20_000,
      label: 'the verdict to flip',
    })
    const res = await this.server._fetch('/hello', { host: 'health.e2e.local' })
    assert.equal(res.status, 504, 'an unhealthy backend is excluded from routing')
  }

  async aReturningBackendRecoversAndTrafficFlowsAgain() {
    await this.backend._restart()
    await waitFor(async () => (await this._health())?.backend_healthy === true, {
      timeoutMs: 20_000,
      label: 'the verdict to recover',
    })
    await waitFor(
      async () => (await this.server._fetch('/hello', { host: 'health.e2e.local' })).status === 200,
      { label: 'traffic to flow again' },
    )
  }
}

export class ProbeTimingSpec extends Test({
  timeout: 120_000,
  after: () => [BackendHealthSpec],
  dependencies: {
    server: () => HealthServer,
    backend: () => HealthBackend,
    probed: () => ProbedClient,
    dead: () => DeadBackendClient,
    slow: () => SlowIntervalClient,
  },
}) {
  async theFirstProbeFiresAtOnceOnADeadBackend() {
    await this.probed._kill()
    await this.backend._stop()
    await this.dead._start()

    // Threshold 1 plus an immediate first probe: unhealthy well inside one
    // five-second interval.
    const started = Date.now()
    await waitFor(
      async () => {
        const stats = await this.server._api<{ active_clients: ClientView[] }>('/aperio/api/stats')
        return stats.active_clients.some((c) => c.backend_healthy === false)
      },
      { timeoutMs: 20_000, label: 'the dead backend to be caught' },
    )
    assert.ok(
      Date.now() - started < 5_000,
      'it took a whole interval, so the first probe did not fire immediately',
    )
  }

  async aHealthCheckedClientBecomesRoutableViaThatFirstProbe() {
    await this.dead._kill()
    await this.backend._restart()
    await this.slow._start()
    // A thirty-second interval: if it waited for one, or stayed stuck
    // unhealthy, this would never pass.
    await this.slow._waitRoutable('slow.e2e.local', '/hello')
  }
}

export class WaitForBackendSpec extends Test({
  timeout: 120_000,
  after: () => [ProbeTimingSpec],
  dependencies: {
    server: () => HealthServer,
    gate: () => GateBackend,
    client: () => WaitForBackendClient,
  },
}) {
  async aGatedClientStaysOutOfRoutingWhileItsBackendIsDown() {
    await this.gate._stop()
    await this.client._start()
    await sleep(2_000)
    const res = await this.server._fetch('/hello', { host: 'waitgate.e2e.local' })
    // 504 from the server, not the 502 a connection-refused would produce:
    // the client never entered routing at all.
    assert.equal(res.status, 504)
  }

  async theGateOpensOnceTheBackendAccepts() {
    await this.gate._restart()
    await this.client._waitRoutable('waitgate.e2e.local', '/hello')
  }
}
