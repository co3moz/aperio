import { Test } from 'nole'
import { createServer, type Server } from 'node:http'
import { WebSocketServer, WebSocket } from 'ws'
import { freePort } from './env.js'

/**
 * A WebSocket backend that also answers plain HTTP, so the routability probe
 * has something to ask.
 *
 * The bash harness carries two hand-rolled RFC6455 servers and two hand-rolled
 * clients, about 150 lines of framing, masking and SHA-1 accept keys spread
 * across four Python heredocs. None of that is what the phase is about.
 */
export function WsEchoBase(options: Parameters<typeof Test>[0] = {}) {
  return class extends Test({ timeout: 30_000, ...options }) {
    _port = 0
    _url = ''
    _http?: Server
    _wss?: WebSocketServer

    /** Sent the moment the upgrade completes, before the client says
     *  anything. Empty means "say nothing until spoken to". */
    _greeting(): string | null {
      return null
    }

    async hookListen() {
      this._port = await freePort()
      this._url = `http://127.0.0.1:${this._port}`
      this._http = createServer((_req, res) => {
        res.writeHead(200, { 'content-length': '2' }).end('ok')
      })
      this._wss = new WebSocketServer({ server: this._http })
      this._wss.on('connection', (socket: WebSocket) => {
        const greeting = this._greeting()
        if (greeting) socket.send(greeting)
        socket.on('message', (data: Buffer, isBinary: boolean) => {
          const echoed = Buffer.concat([Buffer.from('echo:'), data])
          socket.send(echoed, { binary: isBinary })
        })
      })
      await new Promise<void>((resolve) => this._http!.listen(this._port, '127.0.0.1', resolve))
    }

    async cleanUp() {
      this._wss?.clients.forEach((c) => c.terminate())
      this._wss?.close()
      await new Promise<void>((resolve) => {
        if (!this._http) return resolve()
        this._http.closeAllConnections()
        this._http.close(() => resolve())
      })
    }
  }
}

/**
 * Opens a WebSocket *through* the tunnel and returns the first frame the far
 * side sends back.
 *
 * `send` may be null, for the case that matters most here: a backend that
 * speaks first. Reading a frame without having sent one only succeeds if the
 * greeting emitted right after the backend's 101 survived the window where
 * the visitor's own handshake was still completing.
 */
export function wsProbe(
  serverUrl: string,
  host: string,
  send: string | null,
  { path = '/ws-echo', timeoutMs = 15_000 } = {},
): Promise<string> {
  const url = new URL(serverUrl)
  return new Promise((resolve, reject) => {
    const socket = new WebSocket(`ws://${url.host}${path}`, { headers: { host } })
    const timer = setTimeout(() => {
      socket.terminate()
      reject(new Error(`no frame from ${host}${path} within ${timeoutMs}ms`))
    }, timeoutMs)
    socket.on('open', () => {
      if (send !== null) socket.send(send)
    })
    socket.on('message', (data: Buffer) => {
      clearTimeout(timer)
      // Closed cleanly rather than torn down, so the close path is exercised
      // end to end the way the bash probe does it.
      socket.close()
      resolve(data.toString())
    })
    socket.on('error', (e: Error) => {
      clearTimeout(timer)
      reject(e)
    })
  })
}
