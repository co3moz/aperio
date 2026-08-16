import { Test } from 'nole'
import assert from 'node:assert/strict'
import { BaseServerFor, BaseBackendFor, BaseClientFor, HOST } from './fixtures.js'

/**
 * What this file pins down: the connect-time version gate (#113) refuses the
 * pairing it says it refuses, at the upgrade, and refuses nothing else.
 *
 * The gate ships with its floor equal to the documented promise, so today it
 * turns nobody away. That is precisely why it needs a test that drives a
 * refusal deliberately: a mechanism that never fires in the suite is a
 * mechanism nobody would notice breaking, and the moment it matters is the
 * moment somebody narrows the floor for a real break.
 *
 * The refusal is driven with a raw upgrade request rather than a real old
 * client, because there is no released client old enough to be refused by
 * today's floor. A hand-made request announcing an ancient version is the same
 * thing from the server's side: the header is all it reads.
 */

class PairingServer extends BaseServerFor() {}
class PairingBackend extends BaseBackendFor() {}
class PairingClient extends BaseClientFor(() => PairingServer, () => PairingBackend) {}

/** One upgrade attempt, returning what the server answered.
 *
 * A raw `http.request` rather than `fetch`, which refuses to send `Connection:
 * Upgrade` at all. Both outcomes have to be read: a refusal arrives as an
 * ordinary response, an acceptance as the `upgrade` event with the `101`.
 */
async function upgradeWith(
  base: string,
  token: string,
  extra: Record<string, string>,
): Promise<{ status: number; body: string; headers: Record<string, string> }> {
  const http = await import('node:http')
  const url = new URL('/aperio/ws', base)
  return new Promise((resolve, reject) => {
    const req = http.request({
      hostname: url.hostname,
      port: url.port,
      path: url.pathname,
      headers: {
        authorization: `Bearer ${token}`,
        connection: 'Upgrade',
        upgrade: 'websocket',
        'sec-websocket-version': '13',
        'sec-websocket-key': Buffer.from('0123456789abcdef').toString('base64'),
        ...extra,
      },
    })
    req.on('response', (res) => {
      let body = ''
      res.on('data', (c) => (body += c.toString()))
      res.on('end', () =>
        resolve({
          status: res.statusCode ?? 0,
          body,
          headers: res.headers as Record<string, string>,
        }),
      )
    })
    req.on('upgrade', (res, socket) => {
      socket.destroy()
      resolve({
        status: res.statusCode ?? 101,
        body: '',
        headers: res.headers as Record<string, string>,
      })
    })
    req.on('error', reject)
    req.end()
  })
}

export class PairingGateSpec extends Test({
  timeout: 60_000,
  dependencies: { server: () => PairingServer, client: () => PairingClient },
}) {
  async aClientTooOldIsRefusedAtTheUpgradeWithBothVersionsNamed() {
    const res = await upgradeWith(this.server._url, this.server._token, {
      'x-aperio-release': '0.0.1',
    })
    assert.equal(res.status, 426, `expected Upgrade Required, got ${res.status}: ${res.body}`)
    // The value of the gate is the sentence, not the status: an operator has
    // to be able to act without reproducing anything.
    assert.match(res.body, /0\.0\.1/, res.body)
    assert.match(res.body, /Upgrade the client/, res.body)
  }

  async aClientThatAnnouncesNothingIsAdmitted() {
    // Silence predates the header and is inside the documented window.
    // Reading it as age would take a fleet down on the upgrade that
    // introduced the gate, which is the outage this exists to avoid.
    const res = await upgradeWith(this.server._url, this.server._token, {})
    assert.notEqual(res.status, 426, res.body)
  }

  async aGarbledVersionIsNotEvidenceOfAge() {
    const res = await upgradeWith(this.server._url, this.server._token, {
      'x-aperio-release': 'not-a-version',
    })
    assert.notEqual(res.status, 426, res.body)
  }

  async theServerAnnouncesWhatItIsAndWhatItAccepts() {
    // The other half of the window: only the client can judge whether the
    // server is too old for it, so the server has to say.
    const res = await upgradeWith(this.server._url, this.server._token, {
      'x-aperio-release': '99.0.0',
    })
    assert.notEqual(res.status, 426, 'a newer client is not an unsupported pairing')
    assert.match(res.headers['x-aperio-release'] ?? '', /^\d+\.\d+\.\d+/)
    assert.match(res.headers['x-aperio-min-client'] ?? '', /^\d+\.\d+\.\d+/)
  }

  async theRealClientOfThisBuildIsServedExactlyAsBefore() {
    // The whole promise of shipping the mechanism with the floor where the
    // documentation already puts it: nothing that works today stops working.
    const res = await this.server._fetch('/hello', { headers: { host: HOST } })
    assert.equal(res.status, 200)
  }
}
