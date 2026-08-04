import { Test } from 'nole'
import { spawn, type ChildProcess } from 'node:child_process'
import { mkdtemp, rm, writeFile } from 'node:fs/promises'
import { createWriteStream } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { randomUUID } from 'node:crypto'
import { SERVER_BIN, freePort, waitFor } from './env.js'
import { send, sendRaw, type Fetched, type Options } from './http.js'

/**
 * One `aperio-server` process, owned by the runner.
 *
 * A factory rather than a class, so every `extends AperioServerBase()` is a
 * distinct class: nole keys a dependency instance by class identity, so a
 * distinct class is a distinct process on a distinct port. Two specs that name
 * the *same* class deliberately share one server, which is the decision the
 * bash suite could not express, since there every phase shared port 18100
 * whether it wanted to or not.
 *
 * Every member is `_`-prefixed. Nole collects a class's public methods as
 * tests, including a class reached only as a dependency, so an unprefixed
 * `get()` here would be reported as a passing test named `CacheServer.get()`.
 */
/** What `AperioServerBase` takes on top of nole's own class options. */
export interface ServerOptions {
  /** Environment for the process, for the usual case where it is a constant.
   *
   *  A server whose environment names something only the running instance
   *  knows, the port it was given, the data directory it was handed, cannot
   *  be written here and overrides `_env()` instead. That is two of the
   *  twenty-one, and both are the cases where the bash suite had to hardcode
   *  port 18100 to say the same thing. */
  env?: Record<string, string>
}

export function AperioServerBase(options: Parameters<typeof Test>[0] & ServerOptions = {}) {
  return class extends Test({ timeout: 30_000, ...options }) {
    _port = 0
    _token = ''
    _url = ''
    _dataDir = ''
    _cookie = ''
    _output = ''
    _proc?: ChildProcess

    /** The environment this server runs with.
     *
     *  Returns what `AperioServerBase({ env })` was given. A subclass
     *  overrides it only when the value cannot be known before the instance
     *  exists, and may still reach the constant through `super._env()`. */
    _env(): Record<string, string> {
      return options.env ?? {}
    }

    /** A server-side yaml file, written before the process starts and passed
     *  as `APERIO_SERVER_CONFIG`. Rewriting it is how the hot-reload tests
     *  work, which is why the path is stable for the life of the instance. */
    _configFile(): string | null {
      return null
    }

    async _writeConfig(yaml: string): Promise<void> {
      const path = this._configPath()
      await writeFile(path, yaml)
    }

    _configPath(): string {
      return join(this._dataDir, 'aperio-server.yaml')
    }

    async hookStartServer() {
      this._port = await freePort()
      this._token = `e2e-master-${randomUUID()}`
      this._url = `http://127.0.0.1:${this._port}`
      this._dataDir = await mkdtemp(join(tmpdir(), 'aperio-e2e-'))
      const yaml = this._configFile()
      if (yaml !== null) await this._writeConfig(yaml)
      await this._spawn()
    }

    /** Stops and starts on the same port, token and data directory, which is
     *  what "does this survive a restart" has to mean. */
    async _restart(): Promise<void> {
      await this._stop()
      await this._spawn()
    }

    async _stop(): Promise<void> {
      const proc = this._proc
      this._proc = undefined
      if (!proc?.pid || proc.exitCode !== null || proc.signalCode !== null) return
      proc.kill('SIGTERM')
      await Promise.race([
        new Promise((r) => proc.once('exit', r)),
        new Promise((r) => setTimeout(r, 5_000)),
      ])
    }

    async _spawn(): Promise<void> {
      const log = createWriteStream(join(this._dataDir, 'server.log'), { flags: 'a' })
      this._proc = spawn(SERVER_BIN, {
        env: {
          ...process.env,
          PORT: String(this._port),
          APERIO_SERVER_TOKEN: this._token,
          APERIO_DATA_DIR: this._dataDir,
          APERIO_RANDOM_SUBDOMAIN: '*.e2e.local',
          APERIO_GATEWAY_TIMEOUT: '3',
          APERIO_UPTIME_TICK_SECS: '1',
          APERIO_WEBHOOK_RETRY_SCHEDULE: '0',
          ...(this._configFile() !== null ? { APERIO_SERVER_CONFIG: this._configPath() } : {}),
          ...this._env(),
        },
        stdio: ['ignore', 'pipe', 'pipe'],
      })
      const collect = (c: Buffer) => {
        this._output += c.toString()
        log.write(c)
      }
      this._proc.stdout?.on('data', collect)
      this._proc.stderr?.on('data', collect)
      await waitFor(async () => (await this._fetch('/aperio/health')).status === 200, {
        label: `${this.constructor.name} to come up`,
      })
    }

    _fetch(path: string, options: Options = {}): Promise<Fetched> {
      return send(this._url, path, options)
    }

    async _json<T = unknown>(path: string, options: Options = {}): Promise<T> {
      const res = await this._fetch(path, options)
      if (res.status >= 400) {
        throw new Error(`${options.method ?? 'GET'} ${path} answered ${res.status}: ${res.body}`)
      }
      return JSON.parse(res.body) as T
    }

    /** A logged-in dashboard session, as a `Cookie` header value. Memoized:
     *  logging in is rate limited per IP, and every spec sharing this server
     *  shares the one address it dials from. */
    /** Everything the server has printed. Several phases assert on it: a
     *  failover jump and a refused public declaration are stated there and
     *  nowhere else. */
    _log(): string {
      return this._output
    }

    async _waitForLog(needle: string, timeoutMs = 30_000): Promise<void> {
      try {
        await waitFor(() => this._output.includes(needle), { timeoutMs, label: needle })
      } catch {
        throw new Error(
          `the server never logged ${JSON.stringify(needle)}. Its log ends with:\n${this._output.slice(-2000)}`,
        )
      }
    }

    /** Mints a token through the dashboard API and returns its secret. */
    async _mintToken(body: Record<string, unknown>): Promise<{ token: string; id: string }> {
      const cookie = await this._login()
      const made = await this._json<{ token: string; id: string }>('/aperio/api/tokens', {
        method: 'POST',
        headers: { cookie, 'content-type': 'application/json' },
        body: JSON.stringify(body),
      })
      if (!made.token) throw new Error(`no token in the response to ${JSON.stringify(body)}`)
      return made
    }

    /** A dashboard API call with the session already attached. */
    async _api<T = unknown>(path: string, init: Options = {}): Promise<T> {
      const cookie = await this._login()
      return this._json<T>(path, {
        ...init,
        headers: { cookie, 'content-type': 'application/json', ...init.headers },
      })
    }

    /** Waits until the server reports exactly `n` connected clients. Every
     *  phase that starts more than one client needs this: a bind arrives with
     *  a heartbeat, so "routable" and "all of them are up" are different
     *  moments. */
    async _waitForClients(n: number): Promise<void> {
      await waitFor(
        async () => {
          const health = await this._json<{ connected_clients: number }>('/aperio/health')
          return health.connected_clients === n
        },
        { label: `${n} connected client(s)` },
      )
    }

    async _login(): Promise<string> {
      if (this._cookie) return this._cookie
      const cookies = await sendRaw(this._url, '/aperio/auth', {
        method: 'POST',
        headers: { authorization: `Basic ${Buffer.from(`aperio:${this._token}`).toString('base64')}` },
      })
      const raw = cookies.at(0)
      if (!raw) throw new Error('dashboard login set no cookie')
      this._cookie = raw.split(';')[0]
      return this._cookie
    }

    async cleanUp() {
      await this._stop()
      if (this._dataDir) await rm(this._dataDir, { recursive: true, force: true })
    }
  }
}
