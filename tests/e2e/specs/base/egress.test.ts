import { Test } from 'nole'
import assert from 'node:assert/strict'
import { spawn } from 'node:child_process'
import { mkdtemp } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { BaseServerFor, BaseBackendFor, BaseClientFor, HOST } from './fixtures.js'
import { AperioClientBase } from '../../lib/client.js'
import { SERVER_BIN, freePort } from '../../lib/env.js'

/**
 * What this file pins down: the environment of a machine inside a company
 * network does not reroute the one request that was never going to the
 * internet.
 *
 * `HTTP_PROXY` and `HTTPS_PROXY` are set on most corporate workstations,
 * because everything else on them needs a proxy to get out. reqwest reads
 * both by default, so before this was fixed the client dutifully asked the
 * company proxy to fetch `http://127.0.0.1:<backend>`, an address only this
 * machine can see. The proxy refused, and the refusal reached the visitor as
 * their own site being broken.
 *
 * It is an e2e phase rather than a unit test because the variable is
 * process-global: the honest way to set it is to hand it to a real client
 * process, which is what this suite already does. Port 1 stands in for the
 * proxy, so a request that goes there fails immediately and unmistakably
 * rather than hanging until a timeout decides the test's duration.
 */

const BLACK_HOLE = 'http://127.0.0.1:1'

class EgressServer extends BaseServerFor() {}
class EgressBackend extends BaseBackendFor() {}

/** The one client of this file, started as if it were behind a company proxy. */
class ProxiedEnvClient extends BaseClientFor(
  () => EgressServer,
  () => EgressBackend,
) {
  _env() {
    return {
      APERIO_HOSTNAME: HOST,
      // Both spellings, because a real machine has both and reqwest reads
      // whichever it finds first.
      HTTP_PROXY: BLACK_HOLE,
      HTTPS_PROXY: BLACK_HOLE,
      http_proxy: BLACK_HOLE,
      https_proxy: BLACK_HOLE,
    }
  }
}

/**
 * Starts a server and waits for it to exit on its own.
 *
 * Bounded rather than open-ended, because the regression this guards against
 * is a server that *does not* refuse: through a helper that waits for exit,
 * that would hang until the phase timed out instead of failing. Here it is
 * killed and reported as "kept running", which says what went wrong.
 */
async function startAndAwaitExit(
  env: Record<string, string>,
  budgetMs = 15_000,
): Promise<{ exited: boolean; code: number | null; out: string }> {
  const dir = await mkdtemp(join(tmpdir(), 'aperio-e2e-egress-'))
  const proc = spawn(SERVER_BIN, [], {
    env: {
      ...process.env,
      APERIO_DATA_DIR: dir,
      // The bare name, as the standard has it for host/port/log_level.
      PORT: String(await freePort()),
      APERIO_SERVER_TOKEN: 'e2e-egress-token-long-enough',
      ...env,
    },
    stdio: ['ignore', 'pipe', 'pipe'],
  })
  let out = ''
  proc.stdout?.on('data', (c: Buffer) => (out += c.toString()))
  proc.stderr?.on('data', (c: Buffer) => (out += c.toString()))

  const code = await new Promise<number | null>((resolve) => {
    const timer = setTimeout(() => resolve(null), budgetMs)
    proc.once('exit', (c) => {
      clearTimeout(timer)
      resolve(c)
    })
  })
  if (code === null) proc.kill('SIGKILL')
  return { exited: code !== null, code, out }
}

export class OutboundPolicyUnderAProxySpec extends Test({ timeout: 90_000 }) {
  async theAmbientProxyEnvironmentIsIgnoredAndSaidSo() {
    // It used to be read by the HTTP client, so a deployment could be proxying
    // its callbacks with nothing saying so. Now nothing reads it, and the
    // server says as much rather than let a working route vanish in silence.
    const res = await startAndAwaitExit(
      { APERIO_OUTBOUND_BLOCK_PRIVATE: '1', HTTPS_PROXY: 'http://proxy.invalid:3128' },
      6_000,
    )
    assert.equal(res.exited, false, `the server refused instead of starting:\n${res.out}`)
    assert.match(res.out, /HTTPS_PROXY[^"]*no longer used/, res.out)
    assert.match(res.out, /APERIO_OUTBOUND_PROXY/, res.out)
  }

  async aConfiguredProxyNamesWhatThePolicyCanStillCover() {
    // The honest half of the reconciliation: an operator who set a policy is
    // told which part of it a proxy takes away, rather than left believing all
    // of it is in force.
    const res = await startAndAwaitExit(
      {
        APERIO_OUTBOUND_BLOCK_PRIVATE: '1',
        APERIO_OUTBOUND_PROXY: 'proxy.invalid:3128',
      },
      6_000,
    )
    assert.equal(res.exited, false, `the server refused instead of starting:\n${res.out}`)
    assert.match(res.out, /Outbound callbacks go through the proxy proxy.invalid:3128/, res.out)
    assert.match(res.out, /cannot cover a hostname's resolved addresses/, res.out)
    assert.match(res.out, /literal addresses only/, res.out)
  }

  async anInvalidProxyRefusesTheStartRatherThanBeingIgnored() {
    // A server told to go through a proxy is on a network where going direct
    // does not work, so dropping an unreadable value would produce a failure
    // whose cause is a typo somewhere else.
    const res = await startAndAwaitExit({ APERIO_OUTBOUND_PROXY: 'https://proxy.invalid:3128' })
    assert.ok(res.exited, `an https:// proxy should refuse the start:\n${res.out}`)
    assert.match(res.out, /APERIO_OUTBOUND_PROXY is invalid/, res.out)
  }

  async aProxyWithoutAPolicyIsNotTheServersBusiness() {
    const res = await startAndAwaitExit({ APERIO_OUTBOUND_PROXY: 'proxy.invalid:3128' }, 6_000)
    assert.equal(res.exited, false, `the server refused a proxy alone:\n${res.out}`)
    assert.doesNotMatch(res.out, /cannot cover/, res.out)
  }

  async aPolicyWithoutAProxyStillComesUpUnqualified() {
    const res = await startAndAwaitExit({ APERIO_OUTBOUND_BLOCK_PRIVATE: '1' }, 6_000)
    assert.equal(res.exited, false, `the policy alone stopped the start:\n${res.out}`)
    assert.match(res.out, /Outbound callback policy active/, res.out)
    // No qualification, because nothing is taken away when the server dials.
    assert.doesNotMatch(res.out, /cannot cover/, res.out)
  }
}

export class ProxyEnvironmentSpec extends Test({
  timeout: 60_000,
  dependencies: { server: () => EgressServer, client: () => ProxiedEnvClient },
}) {
  async theBackendIsReachedDirectlyDespiteTheProxyEnvironment() {
    const res = await this.server._fetch('/hello', { headers: { host: HOST } })
    assert.equal(
      res.status,
      200,
      'the backend answered, so the request did not go to the proxy at port 1',
    )
  }

  async theClientStaysUpRatherThanFailingItsFirstRequest() {
    // Twice, because the failure this guards against is not a race: a proxied
    // client fails every request the same way, so a second one passing is
    // what says the first was not luck.
    for (const path of ['/hello', '/hello?again=1']) {
      const res = await this.server._fetch(path, { headers: { host: HOST } })
      assert.equal(res.status, 200, `${path} was served from the backend`)
    }
  }
}

/**
 * The other direction, and the feature the file is named for: reaching the
 * tunnel server *through* a proxy, on a network that allows no direct
 * outbound connection (`planned_features.md` #117).
 *
 * Proved against a real proxy rather than a mock of one. It is thirty lines:
 * accept, read the `CONNECT` line, open the upstream socket, answer `200`,
 * then pipe bytes both ways. That last part is the whole contract the client
 * depends on, and it is why TLS still runs end to end inside it.
 */
function ConnectProxyFor(opts: { require?: string; refuseWith?: number } = {}) {
  return class extends Test({ timeout: 30_000 }) {
    _port = 0
    /** The request lines seen, so a test can assert a CONNECT happened. */
    _seen: string[] = []
    _server?: import('node:net').Server
    /** Live sockets, so cleanUp can end a piped tunnel that would otherwise
     *  keep the listener open for the rest of the run. */
    _open: import('node:net').Socket[] = []

    async hookListen() {
      const net = await import('node:net')
      this._server = net.createServer((client) => {
        this._open.push(client)
        client.on('error', () => {})
        client.once('data', (chunk: Buffer) => {
          const head = chunk.toString()
          this._seen.push(head.split('\r\n')[0] ?? '')

          if (opts.require) {
            const want = 'Basic ' + Buffer.from(opts.require).toString('base64')
            if (/Proxy-Authorization: ([^\r\n]+)/i.exec(head)?.[1] !== want) {
              client.end('HTTP/1.1 407 Proxy Authentication Required\r\n\r\n')
              return
            }
          }
          if (opts.refuseWith) {
            client.end(`HTTP/1.1 ${opts.refuseWith} Refused\r\n\r\n`)
            return
          }

          const [host, port] = (this._seen[this._seen.length - 1]?.split(' ')[1] ?? '').split(':')
          const upstream = net.connect(Number(port), host, () => {
            this._open.push(upstream)
            client.write('HTTP/1.1 200 Connection established\r\n\r\n')
            client.pipe(upstream)
            upstream.pipe(client)
          })
          upstream.on('error', () => client.destroy())
        })
      })
      await new Promise<void>((r) => this._server!.listen(0, '127.0.0.1', () => r()))
      this._port = (this._server!.address() as { port: number }).port
    }

    async cleanUp() {
      const server = this._server
      this._server = undefined
      for (const sock of this._open.splice(0)) sock.destroy()
      if (!server) return
      await new Promise<void>((r) => server.close(() => r()))
    }
  }
}

class OpenProxy extends ConnectProxyFor() {}
class AuthProxy extends ConnectProxyFor({ require: 'alice:s3cret' }) {}
class RefusingProxy extends ConnectProxyFor({ refuseWith: 403 }) {}

/** A client whose only route to the server is the proxy it is handed. */
function ClientThrough(proxy: () => new () => { _port: number }, value: (port: number) => string) {
  return class extends AperioClientBase({
    dependencies: {
      server: () => EgressServer,
      backend: () => EgressBackend,
      proxy,
    },
  }) {
    declare readonly server: EgressServer
    declare readonly backend: EgressBackend
    declare readonly proxy: { _port: number }

    _serverUrl() {
      return this.server._url
    }
    _serverToken() {
      return this.server._token
    }
    _backendUrl() {
      return this.backend._url
    }
    _hostname(): string | null {
      return HOST
    }
    _readyPath() {
      return '/hello'
    }
    _env() {
      return {
        APERIO_HOSTNAME: HOST,
        APERIO_EGRESS_PROXY: value(this.proxy._port),
      }
    }
  }
}

class ProxiedClient extends ClientThrough(() => OpenProxy, (p) => `127.0.0.1:${p}`) {}
class AuthProxiedClient extends ClientThrough(
  () => AuthProxy,
  (p) => `alice:s3cret@127.0.0.1:${p}`,
) {}
/** Never comes up: the proxy refuses. Started by the test, not by the harness.
 *
 *  `_hostname()` is null on purpose, so `_start` does not spend the routable
 *  budget waiting for a route that cannot arrive. What is being tested is the
 *  message, and waiting twenty seconds to read it would put the whole phase's
 *  duration in the hands of a timeout. */
class RefusedClient extends ClientThrough(() => RefusingProxy, (p) => `127.0.0.1:${p}`) {
  _autoStart() {
    return false
  }
  _hostname() {
    return null
  }
}

export class EgressProxySpec extends Test({
  timeout: 90_000,
  dependencies: {
    server: () => EgressServer,
    proxy: () => OpenProxy,
    client: () => ProxiedClient,
  },
}) {
  async aVisitorIsServedThroughTheTunnelTheProxyOpened() {
    // The whole path, not a log line: the tunnel is only up if a visitor's
    // request comes back from the backend at the other end of it.
    const res = await this.server._fetch('/hello', { headers: { host: HOST } })
    assert.equal(res.status, 200)
    assert.ok(
      this.proxy._seen.some((l) => l.startsWith('CONNECT ')),
      `the proxy saw no CONNECT: ${JSON.stringify(this.proxy._seen)}`,
    )
  }

  async theProxyIsToldWhereToConnectAndNothingMore() {
    const line = this.proxy._seen.find((l) => l.startsWith('CONNECT ')) ?? ''
    // Host and port, so TLS is still ours: a proxy asked for a path would be
    // one reading the traffic.
    assert.match(line, /^CONNECT [^ ]+:\d+ HTTP\/1\.1$/, line)
  }
}

export class AuthenticatedEgressProxySpec extends Test({
  timeout: 90_000,
  dependencies: {
    server: () => EgressServer,
    client: () => AuthProxiedClient,
  },
}) {
  async aCredentialedProxyAdmitsTheTunnel() {
    const res = await this.server._fetch('/hello', { headers: { host: HOST } })
    assert.equal(res.status, 200, 'the proxy accepted the credential and opened the tunnel')
  }

  async theCredentialNeverReachesTheLog() {
    assert.match(this.client._log(), /through the proxy 127\.0\.0\.1:\d+/, this.client._log())
    assert.doesNotMatch(this.client._log(), /s3cret/, 'the credential reached the log')
  }
}

export class RefusedEgressProxySpec extends Test({
  timeout: 90_000,
  dependencies: { client: () => RefusedClient },
}) {
  async aRefusedConnectNamesTheProxyAndTheStatus() {
    // The failure an operator must never get is a dial that fails three
    // layers from the cause, so this asserts the cause is in the message.
    await this.client._start().catch(() => {})
    await this.client._waitForLog('403', 30_000).catch(() => {})
    const log = this.client._log()
    assert.match(log, /403/, log)
    assert.match(log, /127\.0\.0\.1:\d+/, log)
    await this.client._kill()
  }
}
