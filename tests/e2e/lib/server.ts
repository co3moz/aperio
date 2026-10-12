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
/** A part of the server's settings document, nested by section as the
 *  server reads it: `{ cache: { enabled: true } }`. */
export type Settings = Record<string, unknown>

/** What every fixture runs with unless a spec says otherwise: short enough
 *  timeouts and ticks that a test does not wait on production pacing. */
const FIXTURE_SETTINGS: Settings = {
  routing: { random_subdomain: '*.e2e.local' },
  gateway: { timeout: '3s' },
  uptime: { tick: '1s' },
  webhooks: { retry_schedule: ['0s'] },
}

/** `over` merged into `base`, section by section; a list or a scalar in
 *  `over` replaces the one in `base` whole. */
export function mergeSettings(base: Settings, over: Settings): Settings {
  const out: Settings = { ...base }
  for (const [key, value] of Object.entries(over)) {
    const prev = out[key]
    const isObject = (v: unknown) => typeof v === 'object' && v !== null && !Array.isArray(v)
    out[key] = isObject(prev) && isObject(value)
      ? mergeSettings(prev as Settings, value as Settings)
      : value
  }
  return out
}

/** What `AperioServerBase` takes on top of nole's own class options. */
export interface ServerOptions {
  /** Environment for the process: only what the server reads before its
   *  database opens, and the odd process-level knob a spec needs. */
  env?: Record<string, string>
  /** Settings the server starts with, merged over the fixture's own and
   *  applied through `APERIO_INIT_FILE` on the first start.
   *
   *  A server whose settings name something only the running instance knows,
   *  the port it was given, the data directory it was handed, cannot be
   *  written here and overrides `_settings()` instead. */
  settings?: Settings
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

    /** The settings this server starts with, on top of the fixture's own.
     *
     *  Returns what `AperioServerBase({ settings })` was given. A subclass
     *  overrides it only when a value cannot be known before the instance
     *  exists, and may still reach the constant through `super._settings()`. */
    _settings(): Settings {
      return options.settings ?? {}
    }

    /** Other sections of the first-start document (`tokens`, `users`, …),
     *  for a spec that needs a store seeded before the first request. */
    _document(): Record<string, unknown> {
      return {}
    }

    /** Where the first-start document is written. In the data directory, so
     *  it goes with the instance; the server reads it once, into an empty
     *  database, and a restart does not read it again. */
    _initPath(): string {
      return join(this._dataDir, 'server-init.json')
    }

    /** Changes settings on the running server, the way the dashboard does:
     *  reads the document, merges `changes` into it, and writes it back.
     *  Answers the server's response, so a spec can assert a refusal. */
    async _putSettings(changes: Settings): Promise<Fetched> {
      const current = await this._api<{ settings: Settings }>('/aperio/api/settings')
      const cookie = await this._login()
      return this._fetch('/aperio/api/settings', {
        method: 'PUT',
        headers: { cookie, 'content-type': 'application/json' },
        body: JSON.stringify(mergeSettings(current.settings, changes)),
      })
    }

    /** How often this fixture looks while waiting for the server. A hundred
     *  milliseconds is right for a suite that waits for things to settle; a
     *  spec that puts a clock on the server (the startup budget) overrides
     *  it, so the slack it measures is the product's and not the poll's. */
    _pollMs(): number {
      return 100
    }

    /** Where the server's data directory is made.
     *
     *  A hook because one spec needs it somewhere specific: the disk-full case
     *  puts it on a small filesystem it can genuinely fill. */
    async _makeDataDir(): Promise<string> {
      return mkdtemp(join(tmpdir(), 'aperio-e2e-'))
    }

    /** Where the copy of the server's output is written.
     *
     *  Separate from the data directory for the same spec's sake: when the
     *  data directory is a filesystem with no free space left, a log file on
     *  it cannot be written either, and the harness failing to record output
     *  is not the failure under test. */
    _logPath(): string {
      return join(this._dataDir, 'server.log')
    }

    async hookStartServer() {
      this._token = `e2e-master-${randomUUID()}`
      this._dataDir = await this._makeDataDir()
      await this._spawnOnAFreePort()
    }

    /**
     * Starts on a free port, and tries again on another when that port turns
     * out not to have been free after all.
     *
     * `freePort()` binds a socket, closes it, and reports the number, so what
     * it returns is a port that was free a moment ago. Between that moment and
     * this server binding it, another fixture's probe can hand out the same
     * number, and with four workers and a fixture per spec that happens: the
     * loser logs `Failed to bind 0.0.0.0:PORT: Address already in use` and
     * exits, and every spec sharing it then fails on ECONNREFUSED. One run in
     * about thirty, fifteen tests at a time, all of them collateral
     * (`planned_features.md` #150).
     *
     * Retried rather than eliminated, because the alternative is having the
     * server bind `:0` and parse the port back out of its log, which makes
     * every fixture depend on a log line's wording. A collision is rare and
     * independent, so a second draw is enough.
     */
    async _spawnOnAFreePort(): Promise<void> {
      let last: unknown
      for (let attempt = 1; attempt <= 3; attempt++) {
        this._port = await freePort()
        this._url = `http://127.0.0.1:${this._port}`
        try {
          await this._spawn()
          return
        } catch (e) {
          last = e
          await this._stop()
          // Only a lost race is worth a new port. Anything else is this
          // server failing to start, and retrying hides it.
          if (!this._output.includes('Address already in use')) break
        }
      }
      throw last
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
      // Written on every start, since `_settings()` may name the port, which
      // a lost bind race changes. Only the first start reads it.
      await writeFile(
        this._initPath(),
        JSON.stringify({
          settings: mergeSettings(FIXTURE_SETTINGS, this._settings()),
          ...this._document(),
        }),
      )
      const log = createWriteStream(this._logPath(), { flags: 'a' })
      this._proc = spawn(SERVER_BIN, {
        env: {
          ...process.env,
          // Several specs assert info-level startup and routing messages.
          // An operator's RUST_LOG=warn must not hide those observations.
          RUST_LOG: 'info',
          PORT: String(this._port),
          APERIO_SERVER_TOKEN: this._token,
          APERIO_DATA_DIR: this._dataDir,
          APERIO_INIT_FILE: this._initPath(),
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
      // Raced against the child's exit rather than checked inside the poll,
      // because `waitFor` swallows what its check throws. A server that could
      // not bind is gone in milliseconds, and the twenty seconds after that
      // are spent proving what it has already written down.
      let settled = false
      const died = new Promise<never>((_, reject) => {
        this._proc?.once('exit', (code) => {
          if (!settled) reject(new Error(`the server exited (${code ?? 0}) before answering`))
        })
      })
      died.catch(() => {})
      try {
        // Each probe is abandoned after a few polls' worth of time. One sent
        // before the server bound its port is not refused on every platform
        // (see `Options.timeoutMs`), and a probe stuck in a SYN retransmit
        // would otherwise report the retransmit timer as the server's start.
        const probeTimeoutMs = this._pollMs() * 10
        await Promise.race([
          waitFor(
            async () =>
              (await this._fetch('/aperio/health', { timeoutMs: probeTimeoutMs })).status === 200,
            {
              label: `${this.constructor.name} to come up`,
              intervalMs: this._pollMs(),
            },
          ),
          died,
        ])
        settled = true
      } catch (e) {
        settled = true
        // With what it said. The timeout alone is the least useful half of
        // what happened, and the server writes the reason down every time.
        throw new Error(
          `${(e as Error).message}\n--- ${this.constructor.name} log ---\n${this._output.slice(-2000)}`,
        )
      }
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
        // Declaring a route open needs this permission, and since the server
        // closed by default (#108) a token without it cannot serve an ungated
        // route at all. These fixtures mint a token to narrow something else,
        // a topic list, a rate limit, a hostname, and still expect to serve,
        // so the permission is the default here and the one test about a
        // token that lacks it says so explicitly.
        body: JSON.stringify({ allow_public: true, ...body }),
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
          // The count is answered to a credential; a bare probe gets the
          // status only.
          const health = await this._json<{ connected_clients: number }>('/aperio/health', {
            headers: { authorization: `Bearer ${this._token}` },
          })
          return health.connected_clients === n
        },
        { label: `${n} connected client(s)`, intervalMs: this._pollMs() },
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
