import { Test, timeout } from 'nole'
import assert from 'node:assert/strict'
import { randomUUID } from 'node:crypto'
import { execFile } from 'node:child_process'
import { AperioServerBase } from '../../lib/server.js'
import { CLIENT_BIN, SERVER_BIN } from '../../lib/env.js'
import { StandardBackendBase } from '../../lib/backend.js'
import { ClientFor } from '../../lib/client.js'

/**
 * The time budget every step of bringing a tunnel up has to fit in.
 *
 * THIS NUMBER DOES NOT CHANGE. It is not a measurement of what the code
 * happens to do today, it is the promise the code is held to: a server
 * starts, a client connects, a client leaves, a client comes back and
 * answers its first request, each inside three hundred milliseconds. A
 * change that needs the number raised is a change that made the product
 * slower to come up, and the failing phase below is the place that says so.
 * Fix the cause, never the budget.
 *
 * The budget is enforced by nole's own timeout: each phase calls
 * `timeout(BUDGET_MS)` at the moment the clock should start, and the phase
 * fails if it has not finished by then. The harness polls at five
 * milliseconds here rather than its usual hundred, so what is measured is
 * the product and not the harness's own patience.
 */
export const BUDGET_MS = 300

const HOST = 'budget.e2e.local'
/** How often the fixtures look while waiting on a budgeted step. */
const TIGHT_POLL_MS = 5

class BudgetBackend extends StandardBackendBase() {}

/**
 * Prepared by the hook and started by the spec, so the server's start is a
 * phase with the clock on it rather than setup nobody times.
 */
class BudgetServer extends AperioServerBase() {
  /** The clock polls the server every few milliseconds from one address,
   *  which is exactly what the per-IP bucket exists to refuse; the limiter
   *  is not what is being timed. */
  _env() {
    return { APERIO_IP_LIMIT_MAX: '100000', APERIO_IP_LIMIT_REFILL: '10000' }
  }
  async hookStartServer() {
    this._token = `e2e-master-${randomUUID()}`
    this._dataDir = await this._makeDataDir()
    await warm(SERVER_BIN)
  }
  _pollMs() {
    return TIGHT_POLL_MS
  }
}

/** Started by the spec too, and never waits for routability on its own: the
 *  spec measures that itself. */
class BudgetClient extends ClientFor(() => BudgetServer, () => BudgetBackend) {
  _autoStart() {
    return false
  }
  _hostname() {
    return ''
  }
  _env() {
    return { APERIO_HOSTNAME: HOST }
  }
  _pollMs() {
    return TIGHT_POLL_MS
  }
  async hookStartClient() {
    await warm(CLIENT_BIN)
  }
}

/**
 * One untimed launch of a binary before its timed one. The very first run
 * of a freshly built binary pays for the operating system reading and
 * scanning the whole image, a debug build of the server being some fifty
 * megabytes, and that is the launch a developer does right after
 * `cargo build`. It is the machine's cost, not the product's, and it is
 * paid here, off the clock.
 */
function warm(bin: string): Promise<void> {
  return new Promise((resolve, reject) => {
    execFile(bin, ['--version'], (err) => (err ? reject(err) : resolve()))
  })
}

/** Waits for `check`, looking every few milliseconds. The phase's own
 *  timeout is what fails a slow step; the deadline here only stops the loop
 *  from running on after nole has already given up on the phase. */
async function until(check: () => boolean | Promise<boolean>, deadlineMs = 20_000): Promise<void> {
  const deadline = Date.now() + deadlineMs
  for (;;) {
    try {
      if (await check()) return
    } catch {
      // Not yet.
    }
    if (Date.now() >= deadline) throw new Error('gave up waiting')
    await new Promise((r) => setTimeout(r, TIGHT_POLL_MS))
  }
}

/**
 * The startup budget, phase by phase. Each phase is one step of a tunnel's
 * life, timed on its own; the class timeout is generous so that only the
 * budgeted step, started with `timeout(BUDGET_MS)`, is what fails.
 */
export class StartupBudgetSpec extends Test({
  timeout: 30_000,
  // Alone on the machine: a clock on a process start means nothing while
  // three other specs are spawning servers of their own beside it.
  exclusive: true,
  dependencies: {
    backend: () => BudgetBackend,
    server: () => BudgetServer,
    client: () => BudgetClient,
  },
}) {
  /** The server, from process spawn to answering its health probe. */
  async theServerStartsWithinTheBudget() {
    timeout(BUDGET_MS)
    await this.server._spawnOnAFreePort()
    const health = await this.server._fetch('/aperio/health')
    assert.equal(health.status, 200)
  }

  /** A client, from process spawn to the server counting it as connected. */
  async aClientConnectsWithinTheBudget() {
    // Each phase measures its own step and only that: whatever the previous
    // phase left unfinished is waited out here, off the clock, so a slow
    // start fails the start's phase and not the four after it.
    await until(async () => (await this.server._fetch('/aperio/health')).status === 200)
    const seen = this.server._output.length
    timeout(BUDGET_MS)
    await this.client._start()
    await until(() => this.server._output.slice(seen).includes('Tunnel client connected'))
  }

  /** A request through the tunnel that is up, end to end. */
  async aRequestThroughTheTunnelIsAnsweredWithinTheBudget() {
    await this.server._waitForClients(1)
    timeout(BUDGET_MS)
    const res = await this.server._fetch('/hello', { host: HOST })
    assert.equal(res.status, 200)
    assert.equal(res.body, `backend ${this.backend._port} GET /hello`)
  }

  /** The client leaving, from the signal to the server no longer routing to it. */
  async aClientDisconnectsWithinTheBudget() {
    await this.server._waitForClients(1)
    const seen = this.server._output.length
    timeout(BUDGET_MS)
    await this.client._kill()
    await until(() => this.server._output.slice(seen).includes('Tunnel client disconnected'))
  }

  /** The client coming back, and the first request through it, together:
   *  from the client's spawn to the first `200` a visitor gets. Asked as
   *  "keep asking until it is served" rather than "ask once after the
   *  connect log", because the connection exists a few milliseconds before
   *  the client's first heartbeat declares its route, and a request landing
   *  in that gap is refused rather than held (planned_features #168); what
   *  the budget promises is how soon the site answers, not what one request
   *  in that gap sees. */
  async aReconnectedClientAnswersItsFirstRequestWithinTheBudget() {
    await this.server._waitForClients(0)
    timeout(BUDGET_MS)
    await this.client._start()
    await until(async () => (await this.server._fetch('/hello', { host: HOST })).status === 200)
    const res = await this.server._fetch('/hello', { host: HOST })
    assert.equal(res.body, `backend ${this.backend._port} GET /hello`)
  }
}
