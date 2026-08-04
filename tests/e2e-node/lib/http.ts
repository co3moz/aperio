import { request as httpRequest } from 'node:http'

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
    if (options.body) req.write(options.body)
    req.end()
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
