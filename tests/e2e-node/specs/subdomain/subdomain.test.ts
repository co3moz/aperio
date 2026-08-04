import { Test } from 'nole'
import assert from 'node:assert/strict'
import { AperioServerBase } from '../../lib/server.js'

export class SubdomainServer extends AperioServerBase({
  env: {
    APERIO_RANDOM_SUBDOMAIN: '*-pi.e2e.local',
    APERIO_WEBAUTHN_ORIGIN: 'https://tunnel.e2e.local',
  },
}) {}

/** A random-subdomain pattern that is not `*.host` still has to produce a
 *  usable hostname, with the placeholder actually gone. */
export class RandomSubdomainSpec extends Test({
  dependencies: { server: () => SubdomainServer },
}) {
  async aSameLevelPatternGeneratesAHostname() {
    const tunnel = await this.server._json<{ hostname: string }>('/aperio/api/tunnels', {
      method: 'POST',
      headers: {
        authorization: `Bearer ${this.server._token}`,
        'content-type': 'application/json',
      },
      body: JSON.stringify({ name: 'pattern', ttl_seconds: 300 }),
    })
    assert.match(tunnel.hostname, /-pi\.e2e\.local$/)
    assert.doesNotMatch(tunnel.hostname, /\*/, 'the placeholder is fully substituted')
  }
}

/** The passkey surface, and that it never says whether a user exists. */
export class PasskeySurfaceSpec extends Test({
  dependencies: { server: () => SubdomainServer },
}) {
  async theProbeReportsItIsConfigured() {
    const probe = await this.server._json<{ available: boolean }>('/aperio/auth/passkey')
    assert.equal(probe.available, true)
  }

  async anUnknownUserIsRefusedTheSameWayAsAUserWithoutAPasskey() {
    const res = await this.server._fetch('/aperio/auth/passkey/start', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ username: 'nobody' }),
    })
    assert.equal(res.status, 401, 'a 404 here would be a username oracle')
  }

  async aBogusCeremonyIsRejected() {
    const res = await this.server._fetch('/aperio/auth/passkey/finish', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ ceremony_id: 'bogus', credential: {} }),
    })
    assert.ok([400, 422].includes(res.status), `expected 400 or 422, got ${res.status}`)
  }
}
