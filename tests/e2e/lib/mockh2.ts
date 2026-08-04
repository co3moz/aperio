import { Test } from 'nole'
import { execFile, spawn, type ChildProcess } from 'node:child_process'
import { existsSync } from 'node:fs'
import { join } from 'node:path'
import { promisify } from 'node:util'
import { freePort, waitFor } from './env.js'

const run = promisify(execFile)

function mockH2Bin(): string {
  const root = join(import.meta.dirname, '..', '..', '..')
  const base = process.env.MOCK_H2_BIN ?? join(root, 'target', 'debug', 'mock-h2')
  return existsSync(`${base}.exe`) ? `${base}.exe` : base
}

/**
 * The `mock-h2` helper crate, as a resource.
 *
 * Kept as the Rust helper rather than reimplemented in Node: it is what
 * speaks prior-knowledge HTTP/2 with gRPC trailers on both sides of this
 * phase, and rewriting it would be testing a new mock rather than the
 * tunnel.
 */
export function MockH2Base(options: Parameters<typeof Test>[0] = {}) {
  return class extends Test({ timeout: 60_000, ...options }) {
    _port = 0
    _proc?: ChildProcess

    async hookStart() {
      // Built on demand, the way the bash phase built it: it is a helper
      // crate, not a workspace default, so a fresh checkout has the server
      // and the client but not this. Without it the spawn below fails and
      // the failure reads as "the backend never came up".
      if (!existsSync(mockH2Bin())) {
        await run('cargo', ['build', '-p', 'mock-h2'], {
          cwd: join(import.meta.dirname, '..', '..', '..'),
        })
      }
      this._port = await freePort()
      this._proc = spawn(mockH2Bin(), ['server', String(this._port)], {
        stdio: ['ignore', 'ignore', 'ignore'],
      })
      await waitFor(async () => (await probeH2(`http://127.0.0.1:${this._port}/`, 'up')).ok, {
        label: 'the mock HTTP/2 backend to come up',
      })
    }

    _target(): string {
      return `h2c://127.0.0.1:${this._port}`
    }

    async cleanUp() {
      this._proc?.kill('SIGTERM')
    }
  }
}

/** One prior-knowledge HTTP/2 request, through the helper's client mode. */
export async function probeH2(url: string, payload: string) {
  try {
    const { stdout } = await run(mockH2Bin(), ['client', url, payload])
    return { ok: stdout.includes('status=200'), out: stdout }
  } catch {
    return { ok: false, out: '' }
  }
}
