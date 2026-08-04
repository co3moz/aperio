import { Test } from 'nole'
import { createServer, type IncomingMessage, type Server, type ServerResponse } from 'node:http'
import { freePort } from './env.js'

/** Answered by every backend, counted by none. */
export const READY_PATH = '/__ready'

export interface Reply {
  status?: number
  headers?: Record<string, string>
  body?: string | Uint8Array
}

export type Route = (req: IncomingMessage, url: URL) => Reply | Promise<Reply>

/**
 * A mock backend, in the language the assertions are written in.
 *
 * This is the half of the bash suite that reads worst: nineteen Python
 * servers live inside heredocs in shell strings, two levels of quoting deep,
 * with no reuse between them. Here a backend is a class and a variant is an
 * override, so "the same backend but it sends Cache-Control" is three lines
 * rather than a second copy of the server.
 */
export function MockBackendBase(options: Parameters<typeof Test>[0] = {}) {
  return class extends Test({ timeout: 30_000, ...options }) {
    _port = 0
    _url = ''
    /** Requests served, for the tests that count upstream fetches. */
    _hits: string[] = []
    _server?: Server

    /** Path (or `*` catch-all) to what it answers. */
    _routes(): Record<string, Route> {
      return {}
    }

    async hookListen() {
      this._port = await freePort()
      await this._listen()
    }

    /** Binds on the port this backend already owns. */
    async _listen(): Promise<void> {
      this._url = `http://127.0.0.1:${this._port}`
      const routes = this._routes()
      this._server = createServer((req, res) => {
        void this._serve(routes, req, res)
      })
      await new Promise<void>((resolve) => this._server!.listen(this._port, '127.0.0.1', resolve))
    }

    /** Takes the backend down without giving up its port, so a health test
     *  can bring the same address back. In bash this is `kill` on a pid and a
     *  fresh `start_backend` on the same fixed port, which only works because
     *  every port there is fixed. */
    async _stop(): Promise<void> {
      const server = this._server
      this._server = undefined
      if (!server) return
      server.closeAllConnections()
      await new Promise<void>((resolve) => server.close(() => resolve()))
    }

    async _restart(): Promise<void> {
      await this._stop()
      await this._listen()
    }

    async _serve(
      routes: Record<string, Route>,
      req: IncomingMessage,
      res: ServerResponse,
    ): Promise<void> {
      const url = new URL(req.url ?? '/', this._url)
      // The readiness probe travels the whole tunnel, which is the point of
      // it, but it must leave no trace: it is polled until it succeeds, so
      // counting it would make "how many requests reached the backend"
      // depend on how long the client took to connect. The bash phases carry
      // a hand-written `/count` route in each backend for the same reason.
      if (url.pathname === READY_PATH) {
        res.writeHead(200, { 'cache-control': 'no-store', 'content-length': '5' }).end('ready')
        return
      }
      const route = routes[url.pathname] ?? routes['*']
      if (!route) {
        res.writeHead(404).end()
        return
      }
      // Counted before the handler runs, so a slow route is counted at the
      // moment it was entered rather than when it finished: what the
      // single-flight test asks is how many requests *reached* the backend.
      this._hits.push(url.pathname)
      const reply = await route(req, url)
      const body =
        typeof reply.body === 'string' ? Buffer.from(reply.body) : Buffer.from(reply.body ?? '')
      res.writeHead(reply.status ?? 200, {
        'content-type': 'text/plain',
        ...reply.headers,
        'content-length': String(body.byteLength),
      })
      res.end(body)
    }

    _hitsFor(path: string): number {
      return this._hits.filter((p) => p === path).length
    }

    async cleanUp() {
      await this._stop()
    }
  }
}

/**
 * The workhorse backend, matching what `start_backend` answers in the bash
 * harness so a ported phase asserts on the same strings.
 *
 * The body names the port, which is how every load-balancing and failover
 * assertion tells one backend from another.
 */
export function StandardBackendBase(options: Parameters<typeof Test>[0] = {}) {
  return class extends MockBackendBase(options) {
    _routes(): Record<string, Route> {
      return {
        '*': async (req, url) => {
          if (url.pathname.startsWith('/slow')) {
            await new Promise((r) => setTimeout(r, 5_000))
          }
          if (url.pathname.startsWith('/binary')) {
            // Every byte value, twice: a body no text encoding can carry.
            const data = Buffer.from([...Array(256).keys(), ...Array(256).keys()])
            return { body: data, headers: { 'content-type': 'application/octet-stream' } }
          }
          if (url.pathname.startsWith('/echo-headers')) {
            const lines = Object.entries(req.headers)
              .map(([k, v]) => `${k.toLowerCase()}: ${Array.isArray(v) ? v.join(', ') : v}\n`)
              .join('')
            return { body: lines }
          }
          if (req.method === 'POST' && url.pathname.startsWith('/echo-body')) {
            const body = await readBody(req)
            return { body, headers: { 'content-type': 'application/octet-stream' } }
          }
          if (req.method === 'POST') {
            const body = await readBody(req)
            return { body: `backend ${this._port} POST ${url.pathname} body=${body.toString()}` }
          }
          // The whole request target, query string included: what reaches the
          // backend is part of what the proxy is being tested on.
          return { body: `backend ${this._port} ${req.method} ${req.url}` }
        },
      }
    }
  }
}

function readBody(req: IncomingMessage): Promise<Buffer> {
  return new Promise((resolve) => {
    const chunks: Buffer[] = []
    req.on('data', (c: Buffer) => chunks.push(c))
    req.on('end', () => resolve(Buffer.concat(chunks)))
  })
}
