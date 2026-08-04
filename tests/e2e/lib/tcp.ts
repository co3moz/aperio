import { Test } from 'nole'
import { createServer, connect, type Server } from 'node:net'
import { freePort } from './env.js'

/**
 * A raw TCP echo backend: every chunk comes back with `echo:` in front.
 *
 * The first resource here that is not HTTP, which is the question this phase
 * was ported to answer. It needs nothing from the HTTP backend: the shape of
 * a resource is a lifecycle plus an address, and what travels over it is the
 * subclass's business.
 */
export function TcpEchoBase(options: Parameters<typeof Test>[0] = {}) {
  return class extends Test({ timeout: 30_000, ...options }) {
    _port = 0
    _server?: Server

    async hookListen() {
      this._port = await freePort()
      this._server = createServer((socket) => {
        socket.on('data', (chunk: Buffer) =>
          socket.write(Buffer.concat([Buffer.from('echo:'), chunk])),
        )
        socket.on('error', () => socket.destroy())
      })
      await new Promise<void>((resolve) => this._server!.listen(this._port, '127.0.0.1', resolve))
    }

    _address(): string {
      return `127.0.0.1:${this._port}`
    }

    async cleanUp() {
      await new Promise<void>((resolve) => {
        if (!this._server) return resolve()
        this._server.close(() => resolve())
      })
    }
  }
}

/**
 * Sends `message` to a local port and reads the reply.
 *
 * Replaces `tcp_probe.py`, which the bash phase writes to a temp file at
 * runtime and then invokes through `sh -c` inside `retry`, quoted three
 * levels deep.
 */
export function tcpProbe(port: number, message: string, timeoutMs = 15_000): Promise<string> {
  return new Promise((resolve, reject) => {
    const want = 'echo:'.length + Buffer.byteLength(message)
    const chunks: Buffer[] = []
    const socket = connect(port, '127.0.0.1')
    const done = (fn: () => void) => {
      clearTimeout(timer)
      socket.destroy()
      fn()
    }
    const timer = setTimeout(
      () => done(() => reject(new Error(`no echo from port ${port} within ${timeoutMs}ms`))),
      timeoutMs,
    )
    socket.on('connect', () => socket.write(message))
    socket.on('data', (c: Buffer) => {
      chunks.push(c)
      if (Buffer.concat(chunks).byteLength >= want) {
        done(() => resolve(Buffer.concat(chunks).toString('latin1')))
      }
    })
    socket.on('error', (e) => done(() => reject(e)))
    socket.on('end', () => done(() => resolve(Buffer.concat(chunks).toString('latin1'))))
  })
}
