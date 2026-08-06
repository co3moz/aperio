import { Test } from 'nole'
import { connect, createServer, type Server, type Socket } from 'node:net'
import { freePort } from './env.js'

/**
 * A TCP proxy that can be made to misbehave: the tunnel link, under weather.
 *
 * Every other resource here is something the product talks to. This is the
 * *wire between* two of them, and it exists because the failures operators
 * actually report are not "the server was down", they are "the link went
 * away for eleven seconds". A client dials this instead of the server, and a
 * test can then add latency, cut every live connection, or refuse new ones,
 * without touching either process.
 *
 * Deliberately not packet loss: on TCP, dropping bytes is not something the
 * network can do to a stream, it is corruption, and asserting on corrupted
 * frames would be asserting on a case that cannot happen. What a lossy link
 * actually delivers is delay and disconnection, which is what this does.
 */
export function FlakyLinkBase(options: Parameters<typeof Test>[0] = {}) {
  return class extends Test({ timeout: 30_000, ...options }) {
    _port = 0
    _url = ''
    _server?: Server
    /** Both halves of every live pair, so a cut reaches all of them. */
    _pairs = new Set<Socket>()
    /** Added to every forwarded chunk, in each direction. */
    _delayMs = 0
    /** While false, a new connection is accepted and immediately dropped,
     *  which is what a link that is down looks like to a dialler. */
    _accepting = true
    /** How many connections have been accepted, so a test can tell a
     *  reconnection from a connection that never went away. */
    _connections = 0

    /** The port to forward to. Named by the subclass, since it belongs to
     *  another resource this one does not own. */
    _upstreamPort(): number {
      throw new Error('a link subclass must say which port it forwards to')
    }

    async hookListen() {
      this._port = await freePort()
      this._url = `http://127.0.0.1:${this._port}`
      this._server = createServer((downstream) => {
        this._connections += 1
        if (!this._accepting) {
          downstream.destroy()
          return
        }
        const upstream = connect(this._upstreamPort(), '127.0.0.1')
        this._join(downstream, upstream)
      })
      await new Promise<void>((resolve) => this._server!.listen(this._port, '127.0.0.1', resolve))
    }

    _join(a: Socket, b: Socket): void {
      this._pairs.add(a)
      this._pairs.add(b)
      const pipe = (from: Socket, to: Socket) => {
        from.on('data', (chunk: Buffer) => {
          // Same delay for every chunk, so timers fire in the order they were
          // scheduled and the stream stays a stream. A random delay would
          // reorder it, which is the one thing TCP guarantees does not happen.
          if (this._delayMs > 0) setTimeout(() => to.write(chunk), this._delayMs)
          else to.write(chunk)
        })
        from.on('close', () => to.destroy())
        from.on('error', () => to.destroy())
      }
      pipe(a, b)
      pipe(b, a)
      const forget = () => {
        this._pairs.delete(a)
        this._pairs.delete(b)
      }
      a.on('close', forget)
      b.on('close', forget)
    }

    /** Cuts every live connection, the way a link going down does: no FIN,
     *  no close frame, just bytes that stop arriving. */
    _sever(): void {
      for (const socket of this._pairs) socket.destroy()
      this._pairs.clear()
    }

    /** Down: cut what is live and refuse what dials next. */
    _down(): void {
      this._accepting = false
      this._sever()
    }

    _up(): void {
      this._accepting = true
    }

    async cleanUp() {
      this._sever()
      await new Promise<void>((resolve) => {
        if (!this._server) return resolve()
        this._server.close(() => resolve())
      })
    }
  }
}
