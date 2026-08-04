import { Test } from 'nole'
import assert from 'node:assert/strict'
import { freePort, waitFor } from '../../lib/env.js'
import { tcpProbe } from '../../lib/tcp.js'
import {
  TunnelServer,
  EchoBackend,
  DeclaringClient,
  PortOverrideBinder,
  NamedBinder,
  LegacyBridge,
} from './fixtures.js'

interface TunnelView {
  name?: string
  target: string
  protocol: string
  available?: boolean
}

/**
 * What a declared tunnel is, who may see it, and who may serve it.
 *
 * One class and in order: the tokens minted here are what the later steps
 * authenticate with, so this reads top to bottom exactly as the bash phase
 * does, without the phase having to be a whole file to say so.
 */
export class TunnelDiscoverySpec extends Test({
  timeout: 90_000,
  dependencies: {
    server: () => TunnelServer,
    echo: () => EchoBackend,
    declaring: () => DeclaringClient,
  },
}) {
  /** Minted by the tests below and read by the ones after them. */
  static plainToken = ''
  static bindToken = ''

  async _mintToken(body: Record<string, unknown>): Promise<string> {
    const cookie = await this.server._login()
    const made = await this.server._json<{ token: string }>('/aperio/api/tokens', {
      method: 'POST',
      headers: { cookie, 'content-type': 'application/json' },
      body: JSON.stringify(body),
    })
    assert.ok(made.token, `no token in the response to ${JSON.stringify(body)}`)
    return made.token
  }

  async theDeclarationsBecomeDiscoverable() {
    const auth = { authorization: `Bearer ${this.server._token}` }
    await waitFor(
      async () => {
        const res = await this.server._fetch(`/aperio/tunnels/${this.declaring._id}`, {
          headers: auth,
        })
        return res.status === 200 && res.body.includes(this.echo._address())
      },
      { label: 'the declared tunnels to be discoverable' },
    )

    const tunnels = await this.server._json<TunnelView[]>(
      `/aperio/tunnels/${this.declaring._id}`,
      { headers: auth },
    )
    // Asked of the parsed document rather than of its text: the bash phase
    // greps for `"protocol":"tcp"`, which also matches `"tcp/udp"` and would
    // pass if the two declarations were swapped.
    const main = tunnels.find((t) => t.target === this.echo._address() && t.protocol === 'tcp')
    assert.ok(main, `no tcp tunnel for ${this.echo._address()} in ${JSON.stringify(tunnels)}`)
  }

  async anUnknownClientIdIsNotFound() {
    const res = await this.server._fetch('/aperio/tunnels/no-such-client', {
      headers: { authorization: `Bearer ${this.server._token}` },
    })
    assert.equal(res.status, 404)
  }

  async discoveryWithoutATokenIsRejected() {
    const res = await this.server._fetch(`/aperio/tunnels/${this.declaring._id}`)
    assert.equal(res.status, 401)
  }

  async aTokenWithoutAllowBindIsRefused() {
    TunnelDiscoverySpec.plainToken = await this._mintToken({ name: 'other-token' })
    const res = await this.server._fetch(`/aperio/tunnels/${this.declaring._id}`, {
      headers: { authorization: `Bearer ${TunnelDiscoverySpec.plainToken}` },
    })
    assert.equal(res.status, 403)
  }

  async theListingNamesEachTunnelAndWhetherItCanBeServed() {
    const listed = await this.server._json<TunnelView[]>('/aperio/tunnels', {
      headers: { authorization: `Bearer ${this.server._token}` },
    })
    const main = listed.find((t) => t.name === 'echo_main')
    assert.ok(main, `echo_main missing from ${JSON.stringify(listed)}`)
    assert.equal(main.available, true)
    // One `tcp/udp` declaration is one tunnel, not two.
    const both = listed.filter((t) => t.name === 'echo_both')
    assert.equal(both.length, 1)
    assert.equal(both[0].protocol, 'tcp/udp')
  }

  async aTokenWithoutAllowBindListsNothing() {
    const listed = await this.server._json<TunnelView[]>('/aperio/tunnels', {
      headers: { authorization: `Bearer ${TunnelDiscoverySpec.plainToken}` },
    })
    // Empty rather than refused: a token is not told what it cannot have.
    assert.deepEqual(listed, [])
  }

  async allowBindOpensTheListingAndThePeersDeclarations() {
    TunnelDiscoverySpec.bindToken = await this._mintToken({
      name: 'binder-token',
      allow_bind: true,
    })
    const auth = { authorization: `Bearer ${TunnelDiscoverySpec.bindToken}` }

    const listed = await this.server._json<TunnelView[]>('/aperio/tunnels', { headers: auth })
    assert.ok(listed.some((t) => t.name === 'echo_main'))

    const res = await this.server._fetch(`/aperio/tunnels/${this.declaring._id}`, {
      headers: auth,
    })
    assert.equal(res.status, 200, 'allow_bind may read a peer’s declarations')
  }

  async theDashboardListsThem() {
    const cookie = await this.server._login()
    const listed = await this.server._json<TunnelView[]>('/aperio/api/tunnels', {
      headers: { cookie },
    })
    assert.ok(listed.some((t) => t.name === 'echo_main'))
  }
}

/** Binding a declared tunnel, three ways, each carrying bytes end to end. */
export class TunnelBindingSpec extends Test({
  timeout: 120_000,
  after: () => [TunnelDiscoverySpec],
  dependencies: {
    server: () => TunnelServer,
    echo: () => EchoBackend,
    declaring: () => DeclaringClient,
    override: () => PortOverrideBinder,
    named: () => NamedBinder,
    legacy: () => LegacyBridge,
  },
}) {
  async _echoes(port: number, message: string) {
    await waitFor(
      async () => (await tcpProbe(port, 'ping')).includes('echo:ping'),
      { label: `the tunnel on ${port} to carry bytes` },
    )
    assert.match(await tcpProbe(port, message), new RegExp(`echo:${message}`))
  }

  async byClientIdWithAPortOverride() {
    this.override._localPort = await freePort()
    await this.override._start()
    await this._echoes(this.override._localPort, 'ping-123')
    await this.override._waitForLog(`Tunnel bound: 127.0.0.1:${this.override._localPort}`)
  }

  async byNameWithATokenThatOnlyCarriesAllowBind() {
    this.named._bindToken = TunnelDiscoverySpec.bindToken
    assert.ok(this.named._bindToken, 'the allow_bind token was never minted')
    this.named._mainPort = await freePort()
    this.named._bothPort = await freePort()
    await this.named._start()
    await this._echoes(this.named._mainPort, 'ping-named')
    await this.named._waitForLog('tunnel echo_main')
  }

  async aTcpUdpDeclarationOpensBothHalvesOnOnePort() {
    const line = (proto: string) =>
      `127.0.0.1:${this.named._bothPort} -> tunnel echo_both -> ${this.echo._address()} (${proto})`
    await this.named._waitForLog(line('tcp'))
    await this.named._waitForLog(line('udp'))
    await this._echoes(this.named._bothPort, 'ping-both')
  }

  async theLegacyTcpBridgeStillCarriesBytes() {
    this.legacy._localPort = await freePort()
    await this.legacy._start()
    await this._echoes(this.legacy._localPort, 'ping-legacy')
  }
}
