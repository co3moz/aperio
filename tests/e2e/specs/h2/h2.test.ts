import { Test } from 'nole'
import assert from 'node:assert/strict'
import { AperioServerBase } from '../../lib/server.js'
import { ClientFor } from '../../lib/client.js'
import { MockH2Base, probeH2 } from '../../lib/mockh2.js'
import { waitFor } from '../../lib/env.js'

/** No random subdomains and no bind: this phase's only client serves
 *  everything, so the visitor needs no Host override. */
export class H2Server extends AperioServerBase({ env: { APERIO_RANDOM_SUBDOMAIN: '' } }) {}

export class H2Backend extends MockH2Base() {}

export class H2Client extends ClientFor(() => H2Server, () => H2Backend) {
  _env() {
    // `target_health: /` against an h2c target means the standard
    // grpc.health.v1.Health/Check RPC, not a GET, which a prior-knowledge
    // HTTP/2 server would refuse.
    return { APERIO_TARGET: this.backend._target(), APERIO_TARGET_HEALTH: '/' }
  }
}

export class Http2TunnelSpec extends Test({
  timeout: 90_000,
  dependencies: {
    server: () => H2Server,
    backend: () => H2Backend,
    client: () => H2Client,
  },
}) {
  async anH2cRequestRoundTripsWithItsTrailers() {
    // The client stays out of routing until its first health probe passes,
    // so reaching this at all is the assertion that the probe answered.
    await waitFor(async () => (await probeH2(`${this.server._url}/echo`, 'ping')).ok, {
      label: 'the h2c tunnel to become routable',
    })

    const { out } = await probeH2(`${this.server._url}/echo`, 'grpc-payload-123')
    assert.match(out, /status=200/)
    assert.match(out, /body=h2-echo:grpc-payload-123/, 'the body reached the HTTP/2 backend')
    assert.match(out, /trailer grpc-status=0/, 'the status trailer is relayed to the visitor')
    assert.match(out, /trailer grpc-message=ok/)
  }

  async theBackendIsProbedOverGrpcHealthChecking() {
    await this.client._waitForLog(`gRPC health of ${this.backend._target()}`)
    // Which line announces the pass depends on whether the very first probe
    // won the race with the backend's listener; either proves SERVING.
    assert.match(this.client._log(), /Backend healthy:|Backend health restored:/)
  }
}
