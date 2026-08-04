import { Test } from 'nole'
import { createServer, type Server } from 'node:http'
import { mkdtemp, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { waitFor } from './env.js'
import { existsSync } from 'node:fs'

/** An HTTP backend on a unix socket, for `unix://` targets. */
export function UnixBackendBase(options: Parameters<typeof Test>[0] = {}) {
  return class extends Test({ timeout: 30_000, ...options }) {
    _socket = ''
    _dir = ''
    _server?: Server

    async hookListen() {
      this._dir = await mkdtemp(join(tmpdir(), 'aperio-uds-'))
      this._socket = join(this._dir, 'backend.sock')
      this._server = createServer((req, res) => {
        const body = `uds backend ${req.method} ${req.url}`
        res.writeHead(200, { 'content-length': String(Buffer.byteLength(body)) }).end(body)
      })
      await new Promise<void>((resolve) => this._server!.listen(this._socket, resolve))
      await waitFor(() => existsSync(this._socket), { label: 'the unix socket to appear' })
    }

    _target(): string {
      return `unix://${this._socket}`
    }

    async cleanUp() {
      await new Promise<void>((resolve) => {
        if (!this._server) return resolve()
        this._server.close(() => resolve())
      })
      if (this._dir) await rm(this._dir, { recursive: true, force: true })
    }
  }
}
