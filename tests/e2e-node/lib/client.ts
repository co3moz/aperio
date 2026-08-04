import { Test } from 'nole'
import { spawn, type ChildProcess } from 'node:child_process'
import { mkdtemp, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { CLIENT_BIN, waitFor } from './env.js'
import { send } from './http.js'
import { READY_PATH } from './backend.js'

/**
 * One `aperio-client` process.
 *
 * Three things this phase needed that the cache phase did not, and all three
 * are the reason it was worth porting second:
 *
 *  - a **config file** instead of env, because a `tunnels:` list and a
 *    `bind-tunnels:` map are not flat scalars,
 *  - **argv**, because `--bind-tunnels <id>` and the legacy `tcp <port>` form
 *    are not settings at all,
 *  - the **log**, because what a binder did is stated in what it printed and
 *    nowhere else.
 *
 * The bash phase writes each config with a `cat > file <<YAML` heredoc and
 * reads each log with `assert_contains "$(cat "$LOG_DIR/...")"`. Both are
 * fine once and tedious five times.
 */
/** What a client needs from the server it dials. */
export interface ServerLike {
  _url: string
  _token: string
}

/** What it needs from the thing it forwards to, when that is an HTTP one. */
export interface BackendLike {
  _url: string
}

type Ctor<T> = new () => T

/**
 * Declares a client's server and backend once, in the `extends` clause.
 *
 * ```ts
 * export class CacheClient extends ClientFor(() => CacheServer, () => CacheBackend) {
 *   _hostname() { return 'cache.e2e.local' }
 *   _env() { return { APERIO_HOSTNAME: 'cache.e2e.local' } }
 * }
 * ```
 *
 * Every client subclass used to name its dependencies and then spell out
 * three getters reading `_url` and `_token` off them, which is the same four
 * lines thirty-three times: the relationship was stated twice and the second
 * statement never said anything the first did not.
 *
 * A client whose shape does not fit, more than one backend, a target that is
 * a socket path rather than a URL, still extends `AperioClientBase` directly
 * and wires what it needs. The helper is for the common case, not a fence.
 */
export function ClientFor<S extends Ctor<ServerLike>, B extends Ctor<object>>(
  server: () => S,
  backend: () => B,
  options: Parameters<typeof Test>[0] = {},
) {
  return class extends AperioClientBase({ ...options, dependencies: { server, backend } }) {
    // `declare` so this is a type, not a field: an emitted field would
    // shadow the instance nole injects.
    declare readonly server: InstanceType<S>
    declare readonly backend: InstanceType<B>

    _serverUrl() {
      return this.server._url
    }
    _serverToken() {
      return this.server._token
    }
    /** The backend's URL, for the usual case where it has one.
     *
     *  `null` when it does not: an `h2c://` target or a unix socket path is
     *  not a URL this can read off the backend, so those clients name their
     *  own target in `_env()` and the base leaves `APERIO_TARGET` unset
     *  rather than setting it to nothing. */
    _backendUrl(): string | null {
      return (this.backend as { _url?: string })._url ?? null
    }
  }
}

/** The same, for a client that has a server but no HTTP backend of its own:
 *  one that serves a directory, or dials a socket it names itself. */
export function ClientOf<S extends Ctor<ServerLike>>(
  server: () => S,
  options: Parameters<typeof Test>[0] = {},
) {
  return class extends AperioClientBase({ ...options, dependencies: { server } }) {
    declare readonly server: InstanceType<S>

    _serverUrl() {
      return this.server._url
    }
    _serverToken() {
      return this.server._token
    }
  }
}

export function AperioClientBase(options: Parameters<typeof Test>[0] = {}) {
  return class extends Test({ timeout: 60_000, ...options }) {
    _proc?: ChildProcess
    _output = ''
    _dir = ''

    /** Env pairs, for a client configured the flat way. */
    _env(): Record<string, string> {
      return {}
    }
    /** Yaml, for one that cannot be. Written to a file and passed as --config. */
    _config(): string | null {
      return null
    }
    /** Everything before the config flag: a subcommand, `--bind-tunnels`, … */
    _args(): string[] {
      return []
    }
    /** False for a client that a spec has to start itself, because what it
     *  needs does not exist until a test has made it. */
    _autoStart(): boolean {
      return true
    }

    _serverUrl(): string {
      throw new Error('a client subclass must say which server it dials')
    }
    _serverToken(): string {
      throw new Error('a client subclass must say which token it uses')
    }
    /** Only for the flat form; a config file names its own target. */
    _backendUrl(): string | null {
      return null
    }
    /** Waited for before the tests run, when the client serves a hostname. */
    _hostname(): string | null {
      return null
    }
    _readyPath(): string {
      return READY_PATH
    }

    async hookStartClient() {
      if (this._autoStart()) await this._start()
    }

    async _start(): Promise<void> {
      this._output = ''
      this._dir ||= await mkdtemp(join(tmpdir(), 'aperio-e2e-client-'))
      const args = [...this._args()]
      const yaml = this._config()
      if (yaml) {
        const path = join(this._dir, `config-${args.length}-${Date.now()}.yaml`)
        await writeFile(path, yaml)
        args.push('--config', path)
      }
      const target = this._backendUrl()
      this._proc = spawn(CLIENT_BIN, args, {
        env: {
          ...process.env,
          APERIO_CONNECTIONS: '1',
          APERIO_SERVER_URL: this._serverUrl(),
          APERIO_SERVER_TOKEN: this._serverToken(),
          ...(target ? { APERIO_TARGET: target } : {}),
          ...this._env(),
        },
        stdio: ['ignore', 'pipe', 'pipe'],
      })
      const collect = (c: Buffer) => {
        this._output += c.toString()
      }
      this._proc.stdout?.on('data', collect)
      this._proc.stderr?.on('data', collect)
      const host = this._hostname()
      if (host) await this._waitRoutable(host, this._readyPath())
    }

    async _waitRoutable(host: string, path: string): Promise<void> {
      await waitFor(
        async () => (await send(this._serverUrl(), path, { host })).status < 400,
        { label: `the tunnel for ${host} to become routable` },
      )
    }

    /** Everything the client has printed so far. */
    _log(): string {
      return this._output
    }

    /** Waits until the client says something, and fails saying what it said
     *  instead, which is the part a `grep -q` in a `retry` throws away. */
    async _waitForLog(needle: string, timeoutMs = 30_000): Promise<void> {
      try {
        await waitFor(() => this._output.includes(needle), { timeoutMs, label: needle })
      } catch {
        throw new Error(`the client never logged ${JSON.stringify(needle)}. It logged:\n${this._output}`)
      }
    }

    async _kill(): Promise<void> {
      const proc = this._proc
      this._proc = undefined
      if (!proc?.pid) return
      // A process that has already gone will never emit `exit` again, so
      // waiting for it hangs until the class timeout. That is not academic:
      // a client whose configuration is wrong exits on its own within a
      // second, and then a *failing* test turned into a minute of cleanup
      // and a second, misleading failure on the teardown.
      if (proc.exitCode !== null || proc.signalCode !== null) return
      // SIGTERM, not SIGKILL, and this is not politeness. Under a coverage
      // build the profile data is written by an exit handler, so a killed
      // process contributes nothing and leaves a truncated file that makes
      // the whole merge fail. The bash harness signals the same way.
      proc.kill('SIGTERM')
      const exited = await Promise.race([
        new Promise((r) => proc.once('exit', () => r(true))),
        new Promise((r) => setTimeout(() => r(false), 5_000)),
      ])
      if (!exited) proc.kill('SIGKILL')
    }

    async cleanUp() {
      await this._kill()
    }
  }
}
