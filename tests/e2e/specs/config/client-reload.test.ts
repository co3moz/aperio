import { Test } from 'nole'
import assert from 'node:assert/strict'
import { writeFile } from 'node:fs/promises'
import { waitFor } from '../../lib/env.js'
import { ClientFor } from '../../lib/client.js'
import { BaseServerFor, BaseBackendFor } from '../base/fixtures.js'

/** This file's own trio: the spec rewrites the client's config to drive a
 *  hot reload, which no other file should see. */
/** The stream below runs at forty requests a second for several seconds,
 *  past what the per-IP bucket refills; the reload is what is under test,
 *  not the limiter. */
class ReloadServer extends BaseServerFor() {
  _env() {
    return { ...super._env(), APERIO_IP_LIMIT_MAX: '100000', APERIO_IP_LIMIT_REFILL: '10000' }
  }
}
class ReloadBackend extends BaseBackendFor() {}

const HOST = 'reload.e2e.local'

/** A client written with a config file, so the file can be rewritten. Two
 *  services, so the reload has more than one connection to hand over. */
class ReloadClient extends ClientFor(() => ReloadServer, () => ReloadBackend) {
  _hostname() {
    return HOST
  }
  _config() {
    return this._yaml('v1')
  }
  _yaml(note: string) {
    return [
      'server:',
      `  url: ${this.server._url}`,
      `  token: ${this.server._token}`,
      'services:',
      `  - target: ${this.backend._url}`,
      `    hostname: ${HOST}`,
      '    name: web',
      '    public: true',
      `  - target: ${this.backend._url}`,
      `    hostname: second-${HOST}`,
      '    name: second',
      '    public: true',
      `    custom_name: "${note}"`,
      '',
    ].join('\n')
  }
}

/** A config reload is invisible to visitors (`planned_features.md` #156):
 *  the replacement connections come up before the old ones close, and the
 *  old ones leave no ghost behind. */
export class ClientReloadSpec extends Test({
  timeout: 120_000,
  dependencies: { server: () => ReloadServer, client: () => ReloadClient },
}) {
  async aReloadUnderLoadFailsNoRequestAndLeavesNoGhost() {
    // A steady stream of requests, before, through and after the reload.
    let failures: string[] = []
    let sent = 0
    let stop = false
    const hammer = (async () => {
      while (!stop) {
        sent += 1
        const res = await this.server._fetch('/hello', { host: HOST })
        if (res.status !== 200) failures.push(`request ${sent} answered ${res.status}`)
        await new Promise((r) => setTimeout(r, 25))
      }
    })()
    // Let it warm up, then rewrite the config: the watcher sees the change
    // and the supervisor hands the connections over.
    await new Promise((r) => setTimeout(r, 500))
    const seenBefore = this.client._output.length
    await writeFile(this.client._configPath, this.client._yaml('v2'))
    await waitFor(
      () => this.client._output.slice(seenBefore).includes('Configuration reloaded'),
      { timeoutMs: 30_000, label: 'the client reloading its configuration' },
    )
    // Keep hammering a little past the handover, then stop.
    await new Promise((r) => setTimeout(r, 1_500))
    stop = true
    await hammer
    assert.ok(sent > 40, `the stream ran through the reload (${sent} requests)`)
    assert.deepEqual(failures, [], 'no request failed across the reload')
    // The replacements came up before the old connections were closed.
    const log = this.client._output.slice(seenBefore)
    assert.ok(
      !log.includes('did not come up within'),
      `the handover completed inside its budget:\n${log}`,
    )
    // And the old connections are gone: two services, two connections, no
    // ghost holding a third slot.
    await waitFor(
      async () => {
        const health = await this.server._json<{ connected_clients: number }>('/aperio/health', {
          headers: { authorization: `Bearer ${this.server._token}` },
        })
        return health.connected_clients === 2
      },
      { timeoutMs: 15_000, label: 'exactly the two replacement connections' },
    )
  }
}
