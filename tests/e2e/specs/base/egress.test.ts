import { Test } from 'nole'
import assert from 'node:assert/strict'
import { spawn } from 'node:child_process'
import { mkdtemp } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { BaseServerFor, BaseBackendFor, BaseClientFor, HOST } from './fixtures.js'
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
  async theServerRefusesToRunAPolicyItCannotEnforce() {
    const res = await startAndAwaitExit({
      APERIO_OUTBOUND_BLOCK_PRIVATE: '1',
      HTTPS_PROXY: 'http://proxy.invalid:3128',
    })
    assert.ok(res.exited, `the server kept running instead of refusing:\n${res.out}`)
    assert.match(res.out, /Refusing to start/, res.out)
    // Named, both sides of it: a refusal that does not say which variable and
    // which setting leaves an operator guessing at their own environment.
    assert.match(res.out, /HTTPS_PROXY/, res.out)
    assert.match(res.out, /APERIO_OUTBOUND_BLOCK_PRIVATE/, res.out)
  }

  async everySpellingOfTheVariableIsNoticed() {
    for (const name of ['HTTP_PROXY', 'http_proxy', 'ALL_PROXY']) {
      const res = await startAndAwaitExit({
        APERIO_OUTBOUND_ALLOWLIST: 'hooks.example.com',
        [name]: 'http://proxy.invalid:3128',
      })
      assert.ok(res.exited, `${name} did not stop the start:\n${res.out}`)
      assert.match(res.out, new RegExp(`Refusing to start[^]*${name}`), `${name}: ${res.out}`)
    }
  }

  async aProxyWithoutAPolicyIsNotTheServersBusiness() {
    // The other half of the guard, and the one that keeps it from being an
    // outage generator: the overwhelming majority of servers with a proxy set
    // have no outbound policy at all, and must start exactly as before.
    const res = await startAndAwaitExit(
      { HTTPS_PROXY: 'http://proxy.invalid:3128' },
      6_000,
    )
    assert.equal(res.exited, false, `the server refused a proxy alone:\n${res.out}`)
    assert.doesNotMatch(res.out, /Refusing to start/, res.out)
  }

  async aPolicyWithoutAProxyStillComesUp() {
    const res = await startAndAwaitExit({ APERIO_OUTBOUND_BLOCK_PRIVATE: '1' }, 6_000)
    assert.equal(res.exited, false, `the policy alone stopped the start:\n${res.out}`)
    assert.match(res.out, /Outbound callback policy active/, res.out)
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
