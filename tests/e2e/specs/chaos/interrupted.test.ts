import { Test } from 'nole'
import assert from 'node:assert/strict'
import { send, slowBody, stream } from '../../lib/http.js'
import { sleep, waitFor } from '../../lib/env.js'
import {
  CHAOS_HOST,
  ChaosBackend,
  ChaosClient,
  ChaosServer,
  ReplacementClient,
} from './fixtures.js'

/** Nothing here may take longer than this to settle. A hang is the failure
 *  being tested for, so every wait is bounded and the bound is the assertion. */
const SETTLE_MS = 25_000

/** Reads a stream to its end (or its interruption) and says which happened. */
async function drain(
  s: Awaited<ReturnType<typeof stream>>,
): Promise<{ chunks: number; ended: 'clean' | 'error' }> {
  let chunks = 0
  for (;;) {
    let piece: Buffer | null
    try {
      piece = await s.next()
    } catch {
      return { chunks, ended: 'error' }
    }
    if (piece === null) return { chunks, ended: 'clean' }
    chunks += 1
  }
}

/** Fails unless `work` settles inside `SETTLE_MS`, which is the whole point:
 *  the wrong answer to every case here is "it is still waiting". */
async function within<T>(what: string, work: Promise<T>): Promise<T> {
  const timer = sleep(SETTLE_MS).then(() => {
    throw new Error(`${what} had not settled after ${SETTLE_MS}ms`)
  })
  return Promise.race([work, timer]) as Promise<T>
}

/**
 * A response that is already flowing when the server goes away.
 *
 * The interruption is not the interesting part, a killed process ends its
 * sockets and the visitor finds out. What is worth pinning down is that the
 * visitor finds out *promptly* rather than holding an open connection that
 * will never produce another byte, and that the tunnel re-forms on its own
 * afterwards, without the client being touched.
 */
export class ServerRestartMidStreamSpec extends Test({
  timeout: 120_000,
  dependencies: {
    server: () => ChaosServer,
    backend: () => ChaosBackend,
    client: () => ChaosClient,
  },
}) {
  async aStreamedResponseEndsWhenTheServerRestartsUnderIt() {
    const streamed = await stream(this.server._url, '/stream', { host: CHAOS_HOST })
    assert.equal(streamed.status, 200)
    // Reading one chunk proves the response is genuinely in flight: the
    // headers are out, the backend is writing, and what follows interrupts a
    // transfer rather than a request that had not started.
    const first = await within('the first chunk', streamed.next())
    assert.equal(first?.toString().startsWith('chunk-'), true)

    await this.server._restart()

    const { ended } = await within('the interrupted stream', drain(streamed))
    // Either shape is correct and which one arrives depends on whether the
    // socket was reset or closed. What is not correct is a third outcome,
    // never settling, and `within` is what rules that out.
    assert.ok(ended === 'error' || ended === 'clean')
  }

  async theTunnelComesBackWithoutTouchingTheClient() {
    // Two waits, because they answer different questions. The first is that
    // the client dialled back on its own, which is the "without touching the
    // client" half and fails loudly if it never happens.
    await this.server._waitForClients(1)

    // The second is that the route serves again, and it polls rather than
    // asking once. `connected_clients` counts registrations, so it turns 1 the
    // moment the socket is back, and the request that follows has to beat the
    // three-second gateway timeout this fixture sets. On a loaded runner with
    // instrumented binaries it does not always, and the suite reported a 504
    // for a tunnel that was about to work: an eventual property asserted at a
    // single instant. The claim in this class's own words is that the tunnel
    // "re-forms on its own", and SETTLE_MS is what makes that a claim rather
    // than a hope, exactly as the comment on `within` says.
    let res: Awaited<ReturnType<typeof send>> | undefined
    await within(
      'a request after the restart',
      waitFor(
        async () => {
          res = await send(this.server._url, '/after', { host: CHAOS_HOST })
          return res.status === 200
        },
        { timeoutMs: SETTLE_MS, label: 'the tunnel to serve again' },
      ),
    )
    // Still the assertion that matters: it came back to the *right* backend,
    // which a bare status could not tell us.
    assert.equal(res?.body, `backend ${this.backend._port} GET /after`)
  }
}

/**
 * An upload whose client dies while the body is still being sent.
 *
 * The failover phase covers the other direction, a *response* interrupted by
 * a dying client. This is the upload half, which travels a different path:
 * the body is being streamed towards a client that stops existing halfway
 * through, so the question is whether the sender is told, and how fast.
 */
export class UploadInterruptedSpec extends Test({
  timeout: 120_000,
  dependencies: {
    server: () => ChaosServer,
    backend: () => ChaosBackend,
    client: () => ChaosClient,
    replacement: () => ReplacementClient,
  },
  after: () => [ServerRestartMidStreamSpec],
}) {
  async anUploadWhoseClientDiesMidBodyDoesNotHang() {
    // Declared length, delivered slowly: the request is open and incomplete
    // at the moment the client is killed, which is the state under test. A
    // `body:` here would be handed to the socket in one go and the upload
    // would be over before anything could interrupt it, which is exactly the
    // hole this test fell into on its first run.
    const total = 2 * 1024 * 1024
    const inFlight = send(this.server._url, '/sink', {
      method: 'POST',
      host: CHAOS_HOST,
      headers: { 'content-length': String(total) },
      bodyStream: slowBody(total, 32 * 1024, 120),
    }).then(
      (res) => ({ settled: true as const, status: res.status }),
      () => ({ settled: true as const, status: 0 }),
    )
    await sleep(300)
    await this.client._kill()

    const outcome = await within('the interrupted upload', inFlight)
    assert.equal(outcome.settled, true)
    // A status of 0 is the connection failing, which is a legitimate answer
    // to "the other end went away". A 2xx would be the wrong one: nothing
    // read that body to the end.
    assert.notEqual(outcome.status, 200)
  }

  async aReplacementClientTakesOverAndUploadsWorkAgain() {
    await this.replacement._start()
    await this.server._waitForClients(1)
    const res = await within(
      'an upload after the replacement connected',
      send(this.server._url, '/sink', {
        method: 'POST',
        host: CHAOS_HOST,
        body: Buffer.alloc(64 * 1024, 0x62),
      }),
    )
    assert.equal(res.status, 200)
    assert.equal(res.body, `sank ${64 * 1024}`)
  }
}

/**
 * The backend dying with a response half-delivered.
 *
 * The other direction from the server restart: here the tunnel is fine and
 * what disappears is the thing on the far end of it. The visitor has already
 * received a valid head and part of a body, so there is no status left to
 * change, and the only correct behaviours are to end the transfer and to be
 * ready again once the backend is.
 */
export class BackendDiesMidStreamSpec extends Test({
  timeout: 120_000,
  dependencies: {
    server: () => ChaosServer,
    backend: () => ChaosBackend,
    client: () => ChaosClient,
    replacement: () => ReplacementClient,
  },
  after: () => [SilentBackendSpec],
}) {
  async aStreamEndsWhenTheBackendGoesAwayUnderIt() {
    const streamed = await stream(this.server._url, '/stream', { host: CHAOS_HOST })
    assert.equal(streamed.status, 200)
    const first = await within('the first chunk', streamed.next())
    assert.equal(first?.toString().startsWith('chunk-'), true)

    await this.backend._stop()

    const { chunks, ended } = await within('the stream whose backend died', drain(streamed))
    assert.ok(chunks > 0, 'the visitor had received part of the body before the backend died')
    assert.ok(ended === 'error' || ended === 'clean')
  }

  async theServiceRecoversWhenTheBackendReturns() {
    await this.backend._listen()
    await within(
      'the backend to answer again',
      waitFor(
        async () =>
          (await send(this.server._url, '/back', { host: CHAOS_HOST })).status === 200,
        { label: 'the backend to answer again', timeoutMs: 20_000 },
      ),
    )
  }
}

/**
 * A backend that accepts the connection and then says nothing.
 *
 * The worst-behaved backend is not the one that refuses, it is the one that
 * looks healthy at every layer a probe can see and never answers. Without a
 * timeout this is where a proxy accumulates connections until it stops
 * serving anyone, so what is asserted is that the gateway timeout fires, and
 * that the connection it gave up on did not take the tunnel with it.
 */
export class SilentBackendSpec extends Test({
  timeout: 120_000,
  dependencies: {
    server: () => ChaosServer,
    backend: () => ChaosBackend,
    client: () => ChaosClient,
    replacement: () => ReplacementClient,
  },
  after: () => [UploadInterruptedSpec],
}) {
  async aBackendThatNeverAnswersTimesOutRatherThanHanging() {
    const started = Date.now()
    const res = await within(
      'a request to a silent backend',
      send(this.server._url, '/silent', { host: CHAOS_HOST }),
    )
    assert.ok(res.status >= 500, `expected a 5xx, got ${res.status}`)
    // The bound is `gateway_response_timeout`, four seconds here. The
    // ceiling is generous on purpose: what is asserted is that a bound exists
    // and is roughly the configured one, not that the timer is precise.
    assert.ok(Date.now() - started < 15_000, 'the response timeout did not fire promptly')
  }

  async thePathIsStillGoodAfterwards() {
    const res = await within(
      'a normal request after the timeout',
      send(this.server._url, '/fine', { host: CHAOS_HOST }),
    )
    assert.equal(res.status, 200)
    assert.equal(res.body, `backend ${this.backend._port} GET /fine`)
  }

  async severalSilentRequestsDoNotWedgeTheTunnel() {
    // One timeout releasing its slot is easy; the failure worth catching is a
    // leak, where each abandoned request keeps something and the tunnel stops
    // serving after enough of them.
    await within(
      'three silent requests',
      Promise.all([
        send(this.server._url, '/silent', { host: CHAOS_HOST }),
        send(this.server._url, '/silent', { host: CHAOS_HOST }),
        send(this.server._url, '/silent', { host: CHAOS_HOST }),
      ]),
    )
    const res = await within(
      'a normal request after three timeouts',
      send(this.server._url, '/still-here', { host: CHAOS_HOST }),
    )
    assert.equal(res.status, 200)
  }
}
