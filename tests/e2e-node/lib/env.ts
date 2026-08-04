import { execFileSync } from 'node:child_process'
import { existsSync } from 'node:fs'
import { join } from 'node:path'
import { createServer } from 'node:net'

/** Where cargo put the binaries. Mirrors tests/lib/harness.sh, including the
 *  relocated-target-dir case: a `config.toml` can move it, so cargo is asked
 *  rather than guessed at. */
function targetDir(): string {
  if (process.env.CARGO_TARGET_DIR) return process.env.CARGO_TARGET_DIR
  const root = join(import.meta.dirname, '..', '..', '..')
  try {
    const meta = execFileSync('cargo', ['metadata', '--format-version', '1', '--no-deps'], {
      cwd: root,
      encoding: 'utf8',
      stdio: ['ignore', 'pipe', 'ignore'],
    })
    return JSON.parse(meta).target_directory as string
  } catch {
    return join(root, 'target')
  }
}

/** A binary path, with the Windows `.exe` the bash harness also has to handle. */
function binary(name: string, override?: string): string {
  const base = override ?? join(targetDir(), 'debug', name)
  return existsSync(base) ? base : existsSync(`${base}.exe`) ? `${base}.exe` : base
}

export const SERVER_BIN = binary('aperio-server', process.env.APERIO_SERVER_BIN)
export const CLIENT_BIN = binary('aperio-client', process.env.APERIO_CLIENT_BIN)

/**
 * A port nothing is listening on.
 *
 * The bash suite pins every port (18100 for the server, 18101/2/8 for the
 * backends), which is why its phases must run one at a time and why a server
 * that outlives its phase takes down the *next* one. Asking the OS instead
 * means two suites, or two classes under `--concurrency`, never collide.
 */
export function freePort(): Promise<number> {
  return new Promise((resolve, reject) => {
    const probe = createServer()
    probe.once('error', reject)
    probe.listen(0, '127.0.0.1', () => {
      const { port } = probe.address() as { port: number }
      probe.close(() => resolve(port))
    })
  })
}

/** Polls until `check` returns true, or gives up. Tenth-of-a-second interval
 *  for the same reason the bash `retry` uses one: almost every wait here is
 *  "is it up yet", answered in tens of milliseconds. */
export async function waitFor(
  check: () => Promise<boolean> | boolean,
  { timeoutMs = 20_000, label = 'condition' } = {},
): Promise<void> {
  const deadline = Date.now() + timeoutMs
  for (;;) {
    try {
      if (await check()) return
    } catch {
      // Not up yet.
    }
    if (Date.now() >= deadline) throw new Error(`timed out waiting for ${label}`)
    await new Promise((r) => setTimeout(r, 100))
  }
}

export const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms))
