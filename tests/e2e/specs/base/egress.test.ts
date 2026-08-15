import { Test } from 'nole'
import assert from 'node:assert/strict'
import { BaseServerFor, BaseBackendFor, BaseClientFor, HOST } from './fixtures.js'

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
