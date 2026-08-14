import { Test } from 'nole'
import assert from 'node:assert/strict'
import { mkdtemp } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { AperioServerBase } from '../../lib/server.js'
import { StandardBackendBase } from '../../lib/backend.js'
import { ClientFor } from '../../lib/client.js'
import { send } from '../../lib/http.js'
import { freeBytes, smallFs, smallFsUnsupported, type SmallFs } from '../../lib/smallfs.js'

const DISK_HOST = 'disk.e2e.local'

/**
 * The filesystem under the store, made once and shared by the fixtures below.
 *
 * A module-level value rather than a fixture, because the server needs its
 * mount point *before* it starts and nole resolves fixtures in dependency
 * order, not in the middle of one starting.
 */
let disk: SmallFs | null = null
const unsupported = await smallFsUnsupported()
if (!unsupported) disk = await smallFs(12)

class DiskServer extends AperioServerBase() {
  _outDir = ''

  // The data directory is the small filesystem: the SQLite store, its WAL and
  // the access log all land on the thing that will run out of space. The
  // somewhere-else for the log is made here too, because this is the one hook
  // that runs before the process is spawned and `_logPath` is read.
  async _makeDataDir(): Promise<string> {
    this._outDir = await mkdtemp(join(tmpdir(), 'aperio-disklog-'))
    return disk!.dir
  }

  // The harness's copy of the output does not go on the full filesystem. A
  // test that cannot record what the server said, because the disk it is
  // testing is full, has swapped the failure under test for one of its own.
  _logPath(): string {
    return join(this._outDir, 'server.log')
  }

  _env() {
    return { APERIO_ACCESS_LOG: join(disk!.dir, 'access.jsonl') }
  }

  // The mount is torn down by the spec, not by the base class deleting a
  // directory it did not make.
  async cleanUp() {
    await this._stop()
  }
}

class DiskBackend extends StandardBackendBase() {}

class DiskClient extends ClientFor(
  () => DiskServer,
  () => DiskBackend,
) {
  _hostname() {
    return DISK_HOST
  }
  _readyPath() {
    return '/hello'
  }
  _env() {
    return { APERIO_HOSTNAME: DISK_HOST }
  }
}

/**
 * A disk that fills under the SQLite store (`planned_features.md` #100).
 *
 * The property: **a failed persistence write does not stop the server from
 * proxying.** The store holds tokens, sessions and audit rows; a request being
 * proxied needs none of them, and an operator whose disk filled overnight
 * should find a site that is still up and a log that says why the dashboard
 * cannot save anything.
 *
 * The entry this comes from warned about the way this test goes wrong: if the
 * filesystem is not genuinely full, everything below passes while asserting
 * the property on a path where nothing failed. So the fill is verified twice
 * before the property is touched, once by the free-space count and once by the
 * server itself logging that a write failed. Neither the mechanism nor the
 * assertion is trusted on its own.
 */
export class DiskFullSpec extends Test({
  timeout: 120_000,
  skip: unsupported ?? undefined,
  dependencies: {
    server: () => DiskServer,
    backend: () => DiskBackend,
    client: () => DiskClient,
  },
}) {
  declare server: DiskServer
  declare backend: DiskBackend
  declare client: DiskClient

  async theSiteStaysUpWhenTheStoreCannotBeWrittenTo() {
    // 1. A baseline, so a later 200 means something. If proxying were broken
    //    before the disk filled, everything after this would be a false pass.
    const before = await send(this.server._url, '/hello', { headers: { host: DISK_HOST } })
    assert.equal(before.status, 200, 'the tunnel proxies before the disk fills')

    // 2. Fill it, and check that it is full rather than assuming so.
    const written = await disk!.fill()
    assert.ok(written > 0, 'the filler wrote something')
    assert.equal(await freeBytes(disk!.dir), 0, 'no free space is left on the store’s filesystem')

    // 3. Ask for something that has to be written down. The API call may
    //    succeed or fail; what it must not do is take the server with it.
    let apiAnswered = true
    try {
      await this.server._api('/aperio/api/tokens', {
        method: 'POST',
        body: JSON.stringify({ name: 'written-to-a-full-disk', hostnames: [DISK_HOST] }),
      })
    } catch {
      apiAnswered = false
    }

    // 4. The second proof that the disk really filled: the server itself says
    //    a write failed. Without this the test could pass on a filesystem that
    //    had plenty of room.
    await this.server._waitForLog('Failed to persist', 30_000)

    // 5. The property.
    const after = await send(this.server._url, '/hello', { headers: { host: DISK_HOST } })
    assert.equal(after.status, 200, 'the tunnel still proxies with the store unwritable')
    assert.equal(after.body, before.body, 'and answers with the backend, not an error page')

    // 6. Still a server, not a corpse that happened to answer once.
    const health = await this.server._fetch('/aperio/health')
    assert.equal(health.status, 200, 'the server still reports its health')
    assert.equal(this.server._proc?.exitCode, null, 'the server process did not exit')

    // Not asserted, and worth knowing: at the time of writing the API answers
    // **success** here. The token is in memory, the write failed, and it is
    // gone after a restart. `TokenStore::revoke` already handles this properly
    // (it rolls the removal back and returns false so the caller reports the
    // failure), and `create`/`update` do not. That is its own change, tracked
    // as #114; this spec is about the site staying up, which it does either
    // way, so it records the fact rather than pinning it.
    void apiAnswered
  }

  /** Freeing the space must let the store work again, or the failure was not
   *  the disk being full, it was the store having broken permanently. */
  async andWritesWorkAgainOnceThereIsRoom() {
    await disk!.free()
    assert.ok((await freeBytes(disk!.dir)) > 0, 'space was returned')

    const made = await this.server._api<{ id: string }>('/aperio/api/tokens', {
      method: 'POST',
      body: JSON.stringify({ name: 'written-after-the-disk-was-freed', hostnames: [DISK_HOST] }),
    })
    assert.ok(made.id, 'a token can be created again')

    const listed = await this.server._api<{ name: string }[]>('/aperio/api/tokens')
    assert.ok(
      listed.some((t) => t.name === 'written-after-the-disk-was-freed'),
      'and it is there when the store is read back',
    )
  }

  async cleanUp() {
    if (disk) await disk.cleanup()
  }
}
