import { request as httpRequest, type ClientRequest } from 'node:http'

export interface Fetched {
  status: number
  headers: Record<string, string>
  body: string
  bytes: Buffer
}

export interface Options {
  method?: string
  /** The `Host` header. A tunnel is chosen by it, so most requests set one. */
  host?: string
  headers?: Record<string, string>
  body?: string | Buffer
  /**
   * A body delivered in pieces, over time, rather than all at once.
   *
   * `body` hands the whole thing to `node:http`, which writes it as fast as
   * the socket takes it, so an "upload interrupted halfway" test using it
   * interrupts nothing: the request is already complete before the test can
   * do anything to it. This keeps the request open, which is the state some
   * assertions are entirely about. Set `content-length` yourself, or a
   * chunked request is what gets sent.
   */
  bodyStream?: AsyncIterable<Buffer>
}

/** A body of `total` bytes, `chunk` at a time, `gapMs` apart. */
export async function* slowBody(total: number, chunk: number, gapMs: number): AsyncIterable<Buffer> {
  for (let sent = 0; sent < total; sent += chunk) {
    yield Buffer.alloc(Math.min(chunk, total - sent), 0x61)
    await new Promise((r) => setTimeout(r, gapMs))
  }
}

/**
 * One request, on `node:http` rather than `fetch`.
 *
 * `fetch` silently drops a `Host` header: it is on the forbidden list, so
 * `headers: { host: 'app.e2e.local' }` is accepted, ignored, and the request
 * arrives with the socket's own authority. Almost every assertion in this
 * suite picks its tunnel by `Host`, so on `fetch` they would all have been
 * asking about the wrong service, and quietly.
 */
export function send(base: string, path: string, options: Options = {}): Promise<Fetched> {
  const url = new URL(path, base)
  const headers: Record<string, string> = { ...options.headers }
  if (options.host) headers.host = options.host
  // Declared, not chunked. `node:http` falls back to chunked transfer
  // encoding when no length is given, and a chunked body is a different
  // request: the server cannot refuse it before reading it, so an early 413
  // never happens, and a captured body arrives marked truncated. curl sends
  // a length, so the bash phases were asserting on the other request.
  const body = options.body
  if (body !== undefined && headers['content-length'] === undefined) {
    headers['content-length'] = String(Buffer.byteLength(body))
  }
  return new Promise((resolve, reject) => {
    const req = httpRequest(
      {
        hostname: url.hostname,
        port: url.port,
        path: `${url.pathname}${url.search}`,
        method: options.method ?? 'GET',
        headers,
      },
      (res) => {
        const chunks: Buffer[] = []
        res.on('data', (c: Buffer) => chunks.push(c))
        res.on('end', () => {
          const bytes = Buffer.concat(chunks)
          const flat: Record<string, string> = {}
          for (const [k, v] of Object.entries(res.headers)) {
            flat[k.toLowerCase()] = Array.isArray(v) ? v.join(', ') : (v ?? '')
          }
          resolve({ status: res.statusCode ?? 0, headers: flat, body: bytes.toString(), bytes })
        })
        res.on('error', reject)
      },
    )
    req.on('error', reject)
    writeBody(req, options)
  })
}

/** Writes whatever body the options describe, then ends the request. */
function writeBody(req: ClientRequest, options: Options): void {
  if (options.bodyStream) {
    void (async () => {
      try {
        for await (const chunk of options.bodyStream!) {
          if (req.destroyed || req.writableEnded) return
          req.write(chunk)
        }
        req.end()
      } catch {
        req.destroy()
      }
    })()
    return
  }
  if (options.body) req.write(options.body)
  req.end()
}

/** A response being read chunk by chunk, rather than as a finished body. */
export interface Streamed {
  status: number
  /** Resolves with the next chunk, or `null` once the response ends. A
   *  rejection is the connection failing, which is itself an outcome the
   *  chaos phase asserts on. */
  next(): Promise<Buffer | null>
  close(): void
}

/**
 * Opens a request and hands back the response as it arrives.
 *
 * [`send`] buffers to the last byte, which is exactly what a test about
 * *interruption* cannot use: "the visitor was already reading when the server
 * went away" is a statement about the middle of a response, and a helper that
 * only returns at the end can only say whether the whole thing arrived.
 */
export function stream(base: string, path: string, options: Options = {}): Promise<Streamed> {
  const url = new URL(path, base)
  const headers: Record<string, string> = { ...options.headers }
  if (options.host) headers.host = options.host
  return new Promise((resolve, reject) => {
    const req = httpRequest(
      {
        hostname: url.hostname,
        port: url.port,
        path: `${url.pathname}${url.search}`,
        method: options.method ?? 'GET',
        headers,
      },
      (res) => {
        const queued: Buffer[] = []
        let waiting: ((v: Buffer | null) => void) | undefined
        let failed: ((e: Error) => void) | undefined
        let ended = false
        let error: Error | undefined
        res.on('data', (c: Buffer) => {
          if (waiting) {
            const w = waiting
            waiting = undefined
            failed = undefined
            w(c)
          } else queued.push(c)
        })
        const finish = (e?: Error) => {
          ended = true
          error = e
          if (e && failed) {
            const f = failed
            waiting = undefined
            failed = undefined
            f(e)
          } else if (waiting) {
            const w = waiting
            waiting = undefined
            failed = undefined
            w(null)
          }
        }
        res.on('end', () => finish())
        res.on('error', (e) => finish(e as Error))
        res.on('aborted', () => finish(new Error('response aborted')))
        resolve({
          status: res.statusCode ?? 0,
          next: () =>
            new Promise<Buffer | null>((ok, no) => {
              const queuedChunk = queued.shift()
              if (queuedChunk) return ok(queuedChunk)
              if (error) return no(error)
              if (ended) return ok(null)
              waiting = ok
              failed = no
            }),
          close: () => req.destroy(),
        })
      },
    )
    req.on('error', reject)
    writeBody(req, options)
  })
}

/** Every `set-cookie`, unflattened, for the dashboard session. */
export function sendRaw(base: string, path: string, options: Options = {}): Promise<string[]> {
  const url = new URL(path, base)
  const headers: Record<string, string> = { ...options.headers }
  if (options.host) headers.host = options.host
  return new Promise((resolve, reject) => {
    const req = httpRequest(
      {
        hostname: url.hostname,
        port: url.port,
        path: url.pathname,
        method: options.method ?? 'GET',
        headers,
      },
      (res) => {
        res.resume()
        res.on('end', () => resolve(res.headers['set-cookie'] ?? []))
      },
    )
    req.on('error', reject)
    if (options.body) req.write(options.body)
    req.end()
  })
}
