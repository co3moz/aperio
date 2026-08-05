import { Test } from 'nole'
import assert from 'node:assert/strict'
import { mkdtemp, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { client } from '../../lib/cli.js'
import {
  FeatureServer,
  MainBackend,
  SecondBackend,
  RedirectBackend,
  UdsBackend,
  PositionalClient,
  RedirectClient,
  MultiServiceClient,
  UnixSocketClient,
  HomeConfigClient,
} from './fixtures.js'

export class PositionalTargetSpec extends Test({
  timeout: 60_000,
  dependencies: {
    server: () => FeatureServer,
    backend: () => MainBackend,
    cli: () => PositionalClient,
  },
}) {
  async aPositionalTargetIsProxied() {
    const res = await this.server._fetch('/hello', { host: 'cli.e2e.local' })
    assert.equal(res.body, `backend ${this.backend._port} GET /hello`)
  }
}

/** `check` is the command an operator runs before asking anyone for help. */
export class CheckCommandSpec extends Test({
  timeout: 60_000,
  dependencies: { server: () => FeatureServer, backend: () => MainBackend },
}) {
  async itPassesEndToEndAndSaysWhereEachValueCameFrom() {
    const res = await client(
      ['check', '--server-url', this.server._url, '--server-token', this.server._token],
      { APERIO_TARGET: this.backend._url },
    )
    assert.ok(res.ok, res.stdout + res.stderr)
    assert.match(res.stdout, /All checks passed/)
    assert.match(res.stdout, /WS handshake/, 'the token round-trip is reported')
    assert.match(res.stdout, /\(from CLI argument\)/)
    assert.match(res.stdout, /\(from environment\)/)
  }

  async itFlagsAnUnreachableServerAndTargetAndExitsNonZero() {
    const res = await client(
      ['check', '--server-url', 'http://127.0.0.1:19191', '--server-token', 'bogus'],
      { APERIO_TARGET: 'http://127.0.0.1:19191', APERIO_TARGET_HEALTH: '/health' },
    )
    const out = res.stdout + res.stderr
    assert.match(out, /FAIL {2}server health/)
    assert.match(out, /FAIL {2}target/)
    assert.match(out, /check\(s\) failed/)
    assert.equal(res.ok, false, 'check exits non-zero when something failed')
  }
}

export class RedirectSpec extends Test({
  timeout: 60_000,
  dependencies: {
    server: () => FeatureServer,
    main: () => MainBackend,
    backend: () => RedirectBackend,
    client: () => RedirectClient,
  },
}) {
  async aSameHostRedirectIsFollowedTransparently() {
    const res = await this.server._fetch('/r', { host: 'redir.e2e.local' })
    assert.equal(res.body, `backend ${this.backend._port} GET /hello`)
  }

  async aCrossSiteRedirectPassesThroughToTheVisitor() {
    const res = await this.server._fetch('/ext', { host: 'redir.e2e.local' })
    assert.equal(res.status, 301)
  }
}

/** The id the server assigns has to be the same one the visitor sees. */
export class RequestIdSpec extends Test({
  timeout: 60_000,
  dependencies: {
    server: () => FeatureServer,
    backend: () => MainBackend,
    cli: () => PositionalClient,
  },
}) {
  _seenByBackend(body: string): string[] {
    return body
      .split('\n')
      .filter((l) => l.startsWith('x-request-id: '))
      .map((l) => l.slice('x-request-id: '.length).trim())
  }

  async oneIdReachesBothTheBackendAndTheVisitor() {
    const res = await this.server._fetch('/echo-headers', { host: 'cli.e2e.local' })
    const atBackend = this._seenByBackend(res.body)
    assert.equal(atBackend.length, 1, `the backend saw ${atBackend.length} ids`)
    assert.equal(res.headers['x-request-id'], atBackend[0], 'the visitor sees the same id')
  }

  async anUntrustedInboundIdIsReplacedAndNeverDuplicated() {
    const res = await this.server._fetch('/echo-headers', {
      host: 'cli.e2e.local',
      headers: { 'x-request-id': 'forged-by-visitor' },
    })
    const atBackend = this._seenByBackend(res.body)
    assert.equal(atBackend.length, 1)
    assert.notEqual(atBackend[0], 'forged-by-visitor')
  }

  async inboundAperioHeadersAreStrippedBeforeTheBackend() {
    const res = await this.server._fetch('/echo-headers', {
      host: 'cli.e2e.local',
      headers: { 'x-aperio-org': 'forged', 'x-aperio-client-id': 'forged' },
    })
    assert.doesNotMatch(res.body, /forged/, 'the x-aperio- namespace belongs to the server')
  }
}

export class MultiServiceSpec extends Test({
  timeout: 90_000,
  dependencies: {
    server: () => FeatureServer,
    web: () => MainBackend,
    api: () => SecondBackend,
    client: () => MultiServiceClient,
  },
}) {
  static inboxId = ''

  async eachServiceRoutesToItsOwnBackend() {
    const first = await this.server._fetch('/hello', { host: 'web.e2e.local' })
    assert.match(first.body, new RegExp(`^backend ${this.web._port} `))
    await this.client._waitRoutable('api.e2e.local', '/hello')
    const second = await this.server._fetch('/hello', { host: 'api.e2e.local' })
    assert.match(second.body, new RegExp(`^backend ${this.api._port} `))
  }

  async theStatsShowTheAnnouncedServiceName() {
    const stats = await this.server._api<{ active_clients: { service?: string | null }[] }>(
      '/aperio/api/stats',
    )
    assert.ok(stats.active_clients.some((c) => c.service === 'web'))
  }

  async aCaptureNamesTheServiceThatServedIt() {
    // The inspector used to have only the connection id, which is a uuid:
    // it names the thing that is wrong without saying what it is.
    const logs = await this.server._api<{ id: string; host?: string | null }[]>('/aperio/api/logs')
    const row = logs.find((l) => l.host === 'web.e2e.local')
    assert.ok(row, 'no web.e2e.local request in the log')
    const detail = await this.server._api<{ client_id: string; client_name: string | null }>(
      `/aperio/api/requests/${row.id}`,
    )
    assert.equal(detail.client_name, 'web')
    assert.ok(detail.client_id, 'the id still travels, since it is what an action addresses')
  }

  async aSelectivePurgeRemovesOneHostnameAndLeavesTheOthers() {
    const before = await this.server._api<{ host?: string | null }[]>('/aperio/api/logs')
    assert.ok(before.some((l) => l.host === 'web.e2e.local'), 'the traffic log records the host')

    const purged = await this.server._api<{ status: string }>('/aperio/api/purge', {
      method: 'POST',
      body: JSON.stringify({ hostname: 'web.e2e.local' }),
    })
    assert.equal(purged.status, 'ok')

    const after = await this.server._api<{ host?: string | null }[]>('/aperio/api/logs')
    assert.ok(!after.some((l) => l.host === 'web.e2e.local'), 'the purged host is gone')
    assert.ok(after.some((l) => l.host === 'api.e2e.local'), 'other hosts keep their entries')
  }

  async aPurgeWithoutSelectorsIsRejected() {
    const cookie = await this.server._login()
    const res = await this.server._fetch('/aperio/api/purge', {
      method: 'POST',
      headers: { cookie, 'content-type': 'application/json' },
      body: '{}',
    })
    assert.equal(res.status, 400)
  }

  async anInboundWebhookLandsInTheInbox() {
    await this.server._fetch('/hooks/stripe', {
      host: 'api.e2e.local',
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ event: 'invoice.paid' }),
    })
    const inbox = await this.server._api<{ id: string; uri: string; host: string }[]>(
      '/aperio/api/inbox',
    )
    const entry = inbox.find((e) => e.uri === '/hooks/stripe')
    assert.ok(entry, `no /hooks/stripe entry in ${JSON.stringify(inbox)}`)
    assert.equal(entry.host, 'api.e2e.local')
    MultiServiceSpec.inboxId = entry.id
  }

  async anInboxEntryCanBeReadRefiredAndDeleted() {
    const id = MultiServiceSpec.inboxId
    const detail = await this.server._api<{ headers: unknown }>(`/aperio/api/inbox/${id}`)
    assert.ok(detail.headers, 'the detail carries the headers')

    const refired = await this.server._api<{ status: number }>(
      `/aperio/api/inbox/${id}/refire`,
      { method: 'POST' },
    )
    assert.equal(refired.status, 200, 'the re-fire reached the backend')

    const cookie = await this.server._login()
    await this.server._fetch(`/aperio/api/inbox/${id}`, { method: 'DELETE', headers: { cookie } })
    const after = await this.server._api<{ id: string }[]>('/aperio/api/inbox')
    assert.ok(!after.some((e) => e.id === id))
  }

  async aPerServiceBodyCapRejectsEarlyAndOnlyForThatService() {
    await this.client._waitRoutable('upload.e2e.local', '/hello')
    const small = await this.server._fetch('/hello', {
      host: 'upload.e2e.local',
      method: 'POST',
      body: 'ok',
    })
    assert.equal(small.status, 200)

    const big = 'x'.repeat(200)
    const over = await this.server._fetch('/hello', {
      host: 'upload.e2e.local',
      method: 'POST',
      body: big,
    })
    assert.equal(over.status, 413)

    const elsewhere = await this.server._fetch('/hello', {
      host: 'web.e2e.local',
      method: 'POST',
      body: big,
    })
    assert.equal(elsewhere.status, 200, 'services without a cap keep the global limit')
  }

  async theSecurityHeaderPresetAppliesOnlyWhereItWasAskedFor() {
    const preset = await this.server._fetch('/hello', { host: 'upload.e2e.local' })
    assert.equal(preset.headers['x-frame-options'], 'DENY')
    assert.equal(preset.headers['x-content-type-options'], 'nosniff')
    assert.match(preset.headers['strict-transport-security'] ?? '', /max-age=/)

    const plain = await this.server._fetch('/hello', { host: 'web.e2e.local' })
    assert.equal(plain.headers['x-frame-options'], undefined)
  }
}

export class UnixSocketTargetSpec extends Test({
  timeout: 60_000,
  skip: process.platform === 'win32' ? 'unix sockets are unsupported on Windows' : undefined,
  dependencies: {
    server: () => FeatureServer,
    backend: () => UdsBackend,
    client: () => UnixSocketClient,
  },
}) {
  async requestsAreProxiedOverTheSocket() {
    const res = await this.server._fetch('/uds-hello', { host: 'uds.e2e.local' })
    assert.equal(res.body, 'uds backend GET /uds-hello')
  }
}

export class HomeConfigSpec extends Test({
  timeout: 60_000,
  dependencies: {
    server: () => FeatureServer,
    backend: () => MainBackend,
    client: () => HomeConfigClient,
  },
}) {
  async theUserLevelFileSuppliesTheServerAndTheHealthBlock() {
    const home = await mkdtemp(join(tmpdir(), 'aperio-home-'))
    await writeFile(
      join(home, '.aperio.yaml'),
      [
        'server:',
        `  url: ${this.server._url}`,
        `  token: ${this.server._token}`,
        // The cadence is a genuine default every service inherits, so it
        // belongs in a user-level file. The endpoint is not: it names one
        // backend, and a config file has not described a service at the top
        // level since 0.9.0, so it comes from the environment below.
        'health:',
        '  interval: 1',
        '  timeout: 1',
        '  threshold: 2',
        '',
      ].join('\n'),
    )
    this.client._home = home
    await this.client._start()
    await this.client._waitRoutable('home.e2e.local', '/hello')

    const res = await this.server._fetch('/hello', { host: 'home.e2e.local' })
    assert.match(res.body, new RegExp(`^backend ${this.backend._port} `))
    // Routable at all means a probe from the grouped block passed: a client
    // with a health endpoint starts out of routing until one does.
    await this.client._waitForLog(
      `Backend health check: ${this.backend._url}/health (every 1s, timeout 1s, threshold 2)`,
    )
  }
}
