import { Test } from 'nole'
import assert from 'node:assert/strict'
import { BaseServerFor, BaseBackendFor, BaseClientFor, HOST } from './fixtures.js'

/**
 * What this file pins down: the connect-time version gate refuses, at the
 * upgrade, every pairing that does not share a minor release, in either
 * direction, and a peer that announces nothing or nothing readable; and it
 * admits the same minor. The refusal is a sentence naming both releases and
 * the side to upgrade.
 *
 * Driven with raw upgrade requests rather than real clients of other
 * releases: the header is all the server reads, so a hand-made request
 * announcing a version is the same thing from its side.
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
  /** The server's own release, as its refusal of an ancient client names it. */
  async _serverRelease(): Promise<string> {
    const res = await upgradeWith(this.server._url, this.server._token, {
      'x-aperio-release': '0.0.1',
    })
    const found = /this server is (\d+\.\d+\.\d+)/.exec(res.body)
    assert.ok(found, `the refusal names the server's release: ${res.body}`)
    return found[1]
  }

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

  async aClientThatAnnouncesNothingIsRefusedAsOlder() {
    // Silence predates the rule, which makes it an older minor.
    const res = await upgradeWith(this.server._url, this.server._token, {})
    assert.equal(res.status, 426, res.body)
    assert.match(res.body, /announced no release/, res.body)
    assert.match(res.body, /Upgrade the client/, res.body)
  }

  async aGarbledVersionIsRefused() {
    const res = await upgradeWith(this.server._url, this.server._token, {
      'x-aperio-release': 'not-a-version',
    })
    assert.equal(res.status, 426, res.body)
  }

  async aNewerMinorIsRefusedAndTheServerIsTheOneToUpgrade() {
    const [major, minor] = (await this._serverRelease()).split('.').map(Number)
    const res = await upgradeWith(this.server._url, this.server._token, {
      'x-aperio-release': `${major}.${minor + 1}.0`,
    })
    assert.equal(res.status, 426, res.body)
    assert.match(res.body, /Upgrade the server/, res.body)
  }

  async theSameMinorIsAdmittedAndTheServerSaysWhatItIs() {
    const [major, minor] = (await this._serverRelease()).split('.').map(Number)
    // Another patch of the same minor: the pairing the rule promises.
    const res = await upgradeWith(this.server._url, this.server._token, {
      'x-aperio-release': `${major}.${minor}.99`,
    })
    assert.notEqual(res.status, 426, res.body)
    assert.match(res.headers['x-aperio-release'] ?? '', /^\d+\.\d+\.\d+/)
  }

  async theRealClientOfThisBuildIsServed() {
    const res = await this.server._fetch('/hello', { headers: { host: HOST } })
    assert.equal(res.status, 200)
  }
}
