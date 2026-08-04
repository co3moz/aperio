import { Test } from 'nole'
import assert from 'node:assert/strict'
import { mkdtemp, mkdir, writeFile, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { AperioServerBase } from '../../lib/server.js'
import { StandardBackendBase } from '../../lib/backend.js'
import { ClientFor, ClientOf } from '../../lib/client.js'
import { waitFor } from '../../lib/env.js'

/** Random subdomains off, so an unclaimed hostname really is unclaimed. */
export class MultihostServer extends AperioServerBase() {
  _env() {
    return { APERIO_RANDOM_SUBDOMAIN: '' }
  }
}

export class MultihostBackend extends StandardBackendBase() {}

/** One client, two names, from a comma-separated bind. */
export class TwoNameClient extends ClientFor(() => MultihostServer, () => MultihostBackend) {
  _hostname() {
    return 'one.e2e.local'
  }
  _readyPath() {
    return '/hello'
  }
  _env() {
    return { APERIO_HOSTNAME: 'one.e2e.local,two.e2e.local' }
  }
}

/** One client, two services, each serving its own directory from disk. */
export class ServingClient extends ClientOf(() => MultihostServer) {
  _root = ''

  _autoStart() {
    return false
  }
  _config() {
    return [
      'server:',
      `  url: ${this.server._url}`,
      `  token: ${this.server._token}`,
      'services:',
      '  - name: site_a',
      `    serve: ${join(this._root, 'site_a')}`,
      '    hostname: site-a.e2e.local',
      '  - name: site_b',
      `    serve: ${join(this._root, 'site_b')}`,
      '    hostname: site-b.e2e.local',
      '',
    ].join('\n')
  }
}

export class MultipleHostnamesSpec extends Test({
  timeout: 60_000,
  dependencies: {
    server: () => MultihostServer,
    backend: () => MultihostBackend,
    client: () => TwoNameClient,
  },
}) {
  async bothNamesReachTheOneService() {
    for (const host of ['one.e2e.local', 'two.e2e.local']) {
      const res = await this.server._fetch('/hello', { host })
      assert.equal(res.body, `backend ${this.backend._port} GET /hello`, host)
    }
  }

  async anUnclaimedHostnameIsNotRouted() {
    const res = await this.server._fetch('/hello', { host: 'nope.e2e.local' })
    assert.equal(res.status, 504)
  }
}

export class StaticServeSpec extends Test({
  timeout: 60_000,
  dependencies: { server: () => MultihostServer, client: () => ServingClient },
}) {
  _root = ''

  async before() {
    this._root = await mkdtemp(join(tmpdir(), 'aperio-serve-'))
    for (const [dir, html] of [
      ['site_a', '<h1>site a</h1>\n'],
      ['site_b', '<h1>site b</h1>\n'],
    ]) {
      await mkdir(join(this._root, dir), { recursive: true })
      await writeFile(join(this._root, dir, 'index.html'), html)
    }
    this.client._root = this._root
    await this.client._start()
  }

  async eachHostnameServesItsOwnDirectory() {
    for (const [host, want] of [
      ['site-a.e2e.local', 'site a'],
      ['site-b.e2e.local', 'site b'],
    ]) {
      await waitFor(
        async () => (await this.server._fetch('/', { host })).body.includes(want),
        { label: `${host} to serve its directory` },
      )
    }
  }

  async aRedeployedFileIsServedWithoutARestart() {
    // The file streams from disk, so what is on disk now is what goes out.
    await writeFile(join(this._root, 'site_a', 'index.html'), '<h1>site a v2</h1>\n')
    const res = await this.server._fetch('/', { host: 'site-a.e2e.local' })
    assert.match(res.body, /site a v2/)
  }

  async headReportsTheLengthAGetWouldHaveSent() {
    const res = await this.server._fetch('/', { host: 'site-a.e2e.local', method: 'HEAD' })
    assert.equal(res.headers['content-length'], '19')
  }

  async after() {
    if (this._root) await rm(this._root, { recursive: true, force: true })
  }
}
