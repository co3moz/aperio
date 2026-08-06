import type { IncomingMessage, ServerResponse } from 'node:http'
import { AperioServerBase } from '../../lib/server.js'
import { StandardBackendBase } from '../../lib/backend.js'
import { AperioClientBase, ClientFor } from '../../lib/client.js'
import { FlakyLinkBase } from '../../lib/link.js'
import { waitFor } from '../../lib/env.js'
import { send } from '../../lib/http.js'

/**
 * The chaos phase: what happens when something is interrupted.
 *
 * Every other phase asserts a happy path with a clean shutdown. The failures
 * operators actually meet are the other kind, and each one here pins down a
 * behaviour that was previously a belief: a response cut off mid-stream, an
 * upload whose client dies while it is being sent, a backend that accepts a
 * connection and then says nothing, and a tunnel link that goes away and
 * comes back.
 *
 * What every spec here asserts, in one sentence: an interruption ends in a
 * bounded time with an answer, and the system is still serving afterwards.
 * That second half is the point. Anyone can return 502; the question is
 * whether the next request works.
 */

/**
 * How a slow response is shaped, and why these numbers.
 *
 * The chunk size is not cosmetic. The client **buffers** a response body up
 * to its streaming threshold (256 KB, `STREAM_THRESHOLD` in
 * `aperio-client/src/proxy/http.rs`) and only switches to chunked streaming
 * past it, so a trickle of small pieces arrives at the visitor as one finished
 * response at the very end, and a test written that way asserts nothing about
 * streaming. At 128 KB a piece the threshold is crossed almost immediately
 * and what follows is genuinely a stream in flight.
 */
const STREAM_CHUNKS = 40
const STREAM_CHUNK_BYTES = 128 * 1024
const STREAM_GAP_MS = 200

export const CHAOS_HOST = 'chaos.e2e.local'
export const LINK_HOST = 'link.e2e.local'

/** One numbered piece of the slow response, padded to the chunk size. */
function chunkOf(n: number): string {
  const label = `chunk-${n}:`
  return label + 'x'.repeat(STREAM_CHUNK_BYTES - label.length - 1) + '\n'
}

/** A backend that can be slow, silent, or endless, on purpose. */
export class ChaosBackend extends StandardBackendBase() {
  _rawRoutes(): Record<string, (req: IncomingMessage, res: ServerResponse) => void> {
    return {
      // Arrives in pieces over eight seconds. The first is written
      // immediately, so a reader knows the response has *started* before
      // anything is done to interrupt it.
      '/stream': (_req, res) => {
        res.writeHead(200, { 'content-type': 'text/plain', 'cache-control': 'no-store' })
        let sent = 0
        const timer = setInterval(() => {
          if (sent >= STREAM_CHUNKS || res.writableEnded) {
            clearInterval(timer)
            res.end()
            return
          }
          res.write(chunkOf(sent))
          sent += 1
        }, STREAM_GAP_MS)
        res.on('close', () => clearInterval(timer))
        res.write(chunkOf(0))
        sent = 1
      },
      // Accepts the request and then says nothing, ever. Not an error, not a
      // close: the connection a health check would call established. This is
      // the failure mode a timeout exists for, and the one that hangs a proxy
      // that has none.
      '/silent': () => {},
      // Reads the whole body and reports its size, for the upload cases.
      '/sink': (req, res) => {
        let bytes = 0
        req.on('data', (c: Buffer) => {
          bytes += c.byteLength
        })
        req.on('end', () => {
          const body = `sank ${bytes}`
          res.writeHead(200, { 'content-length': String(body.length) }).end(body)
        })
      },
    }
  }
}

/**
 * Both gateway budgets set low, so "it gives up" is testable in a test rather
 * than in a coffee break.
 *
 * They are two different clocks and the distinction is the reason a silent
 * backend needed a phase of its own: `gateway_timeout` bounds waiting for a
 * *client to connect*, and expires while nothing has been dispatched.
 * `gateway_response_timeout` bounds waiting for a dispatched request to be
 * *answered*, which is the one a backend that accepts and then says nothing
 * runs into. Only the second can end that request.
 */
export class ChaosServer extends AperioServerBase({
  env: { APERIO_GATEWAY_TIMEOUT: '3', APERIO_GATEWAY_RESPONSE_TIMEOUT: '4' },
}) {}

export class ChaosClient extends ClientFor(() => ChaosServer, () => ChaosBackend) {
  _hostname() {
    return CHAOS_HOST
  }
  _env() {
    return { APERIO_HOSTNAME: CHAOS_HOST }
  }
}

/** The replacement for the one a spec kills. Started by the test. */
export class ReplacementClient extends ChaosClient {
  _autoStart() {
    return false
  }
}

// --- The link phase: a client that dials through weather ---------------------

export class LinkServer extends AperioServerBase() {}

export class LinkBackend extends StandardBackendBase() {}

/** Sits between the client and `LinkServer`, and can be taken down. */
export class TunnelLink extends FlakyLinkBase({ dependencies: { server: () => LinkServer } }) {
  declare readonly server: InstanceType<typeof LinkServer>

  _upstreamPort(): number {
    return this.server._port
  }
}

/**
 * Dials the link rather than the server.
 *
 * This one cannot use `ClientFor`: its server for *routing* assertions is
 * `LinkServer`, but the URL it dials is the link's. The two being different
 * is the whole fixture.
 */
export class LinkClient extends AperioClientBase({
  dependencies: { link: () => TunnelLink, backend: () => LinkBackend },
}) {
  declare readonly link: InstanceType<typeof TunnelLink>
  declare readonly backend: InstanceType<typeof LinkBackend>

  _serverUrl() {
    return this.link._url
  }
  _serverToken() {
    return this.link.server._token
  }
  _backendUrl() {
    return this.backend._url
  }
  _hostname() {
    return LINK_HOST
  }
  _env() {
    return { APERIO_HOSTNAME: LINK_HOST }
  }
  /** Routability is checked against the real server, not the link: the
   *  visitor never goes through the link, only the tunnel does. */
  async _waitRoutable(host: string, path: string): Promise<void> {
    await waitFor(async () => (await send(this.link.server._url, path, { host })).status < 400, {
      label: `the tunnel for ${host} to become routable`,
    })
  }
}
