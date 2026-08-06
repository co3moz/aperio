import { Test } from 'nole'
import assert from 'node:assert/strict'
import { send } from '../../lib/http.js'
import { sleep, waitFor } from '../../lib/env.js'
import { LINK_HOST, LinkBackend, LinkClient, LinkServer, TunnelLink } from './fixtures.js'

/**
 * The tunnel link under weather: latency, then a clean disappearance.
 *
 * Every other phase runs the client and the server over a loopback socket
 * that never misbehaves, which is the one network condition no deployment
 * has. The client dials a proxy here instead, so a test can add delay to the
 * link or cut it entirely without either process knowing why.
 *
 * What is asserted is the pair of properties an operator actually depends on:
 * a slow link still serves, and a link that goes away and comes back is
 * recovered from without anybody restarting anything.
 */
export class FlakyLinkSpec extends Test({
  timeout: 180_000,
  dependencies: {
    server: () => LinkServer,
    backend: () => LinkBackend,
    link: () => TunnelLink,
    client: () => LinkClient,
  },
}) {
  async aSlowLinkStillServes() {
    // 120 ms each way on every frame, which is a tunnel crossing an ocean.
    this.link._delayMs = 120
    try {
      const res = await send(this.server._url, '/slow-link', { host: LINK_HOST })
      assert.equal(res.status, 200)
      assert.equal(res.body, `backend ${this.backend._port} GET /slow-link`)
    } finally {
      this.link._delayMs = 0
    }
  }

  async aSeveredLinkIsNoticedAndTheClientReconnects() {
    const before = this.link._connections
    // Down, not just cut: a client that reconnects instantly into a link that
    // is still up would prove nothing about recovering from an outage.
    this.link._down()
    // The visitor's side of an outage: no client, so the server answers
    // rather than hanging. Which 5xx it is belongs to the routing phase.
    await waitFor(
      async () => (await send(this.server._url, '/gone', { host: LINK_HOST })).status >= 500,
      { label: 'the server to notice the client is gone', timeoutMs: 60_000 },
    )
    // Two seconds of a link that refuses, so the client is genuinely in its
    // reconnect loop rather than mid-first-attempt when the link returns.
    await sleep(2_000)
    this.link._up()

    await waitFor(() => this.link._connections > before, {
      label: 'the client to dial again',
      timeoutMs: 60_000,
    })
  }

  async trafficFlowsAgainOnItsOwn() {
    await waitFor(
      async () => (await send(this.server._url, '/recovered', { host: LINK_HOST })).status === 200,
      { label: 'the tunnel to serve again', timeoutMs: 60_000 },
    )
    const res = await send(this.server._url, '/recovered', { host: LINK_HOST })
    assert.equal(res.body, `backend ${this.backend._port} GET /recovered`)
  }

  async theClientSaysItLostTheConnection() {
    // The recovery is only supportable if it is stated somewhere: an operator
    // reading the client log after a blip should find the blip in it.
    assert.match(this.client._log(), /reconnect|disconnect|connection/i)
  }
}
