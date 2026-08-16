import { Test } from 'nole'
import assert from 'node:assert/strict'
import { waitFor } from '../../lib/env.js'
import { ClientFor } from '../../lib/client.js'
import { BaseServerFor, BaseBackendFor, BaseClientFor } from './fixtures.js'

/** This file's own server: the specs below change it, so it is not
 *  shared with another file. See `fixtures.ts`. */
class IsolationServer extends BaseServerFor() {}
class IsolationBackend extends BaseBackendFor() {}
class IsolationClient extends BaseClientFor(() => IsolationServer, () => IsolationBackend) {}

const ORG_HOST = 'orgtraffic.example.com'

export class OrgTrafficClient extends ClientFor(() => IsolationServer, () => IsolationBackend) {
  _token = ''
  _autoStart() {
    return false
  }
  _serverToken() {
    return this._token
  }
  _env() {
    return { APERIO_HOSTNAME: ORG_HOST }
  }
}

/** Three views of one stream, not three samples of it. */
export class ActivityRingsSpec extends Test({
  timeout: 60_000,
  dependencies: { server: () => IsolationServer, client: () => IsolationClient },
}) {
  async everyRangeIsAboutSixtyContiguousCellsOfTheSameTraffic() {
    for (const [range, width, count] of [
      ['15m', 5, 180],
      ['2h', 120, 60],
      ['1d', 900, 96],
    ] as const) {
      const doc = await this.server._api<{
        bucket_secs: number
        buckets: { at: number; total: number }[]
      }>(`/aperio/api/activity?range=${range}`)
      assert.equal(doc.bucket_secs, width, range)
      assert.equal(doc.buckets.length, count, range)
      assert.ok(
        doc.buckets.reduce((n, b) => n + b.total, 0) > 0,
        `${range}: no requests in the ring`,
      )
      const gaps = new Set(
        doc.buckets.slice(1).map((b, i) => b.at - doc.buckets[i].at),
      )
      assert.deepEqual([...gaps], [width], `${range}: slices are not contiguous`)
    }
  }

  async anUnknownRangeIsTheAnswerTheEndpointAlwaysGave() {
    const doc = await this.server._api<{ bucket_secs: number }>('/aperio/api/activity?range=5m')
    assert.equal(doc.bucket_secs, 5, 'an old caller keeps its answer')
  }
}

/** "Why would this be answered that way", without sending a request. */
export class ExplainSpec extends Test({
  // Ordered: this file's specs share one server and change it, so they
  // take turns. Files do not, they have a server each.
  after: () => [ActivityRingsSpec],
  timeout: 60_000,
  dependencies: { server: () => IsolationServer },
}) {
  async itNamesTheOutcomeAndEveryStage() {
    const explanation = await this.server._api<{
      outcome: string
      summary: string
      summary_code: string
      steps: { stage: string; code: string; detail: string }[]
    }>('/aperio/api/explain?hostname=nothing.e2e.local')
    assert.equal(explanation.outcome, 'no_client')
    assert.ok(explanation.summary, 'the one-line answer somebody came for')
    assert.ok(
      explanation.steps.some((s) => s.stage === 'maintenance'),
      'every stage is reported, not only the deciding one',
    )
    // Said twice: an English sentence for a CLI, a code for a screen that
    // has eight languages. A step with only the first ships untranslated.
    assert.equal(explanation.summary_code, 'no_client.504')
    for (const step of explanation.steps) {
      assert.ok(step.detail, `${step.stage} has a sentence`)
      assert.match(step.code, /^[a-z_]+\.[a-z_0-9]+$/, `${step.stage} has a code`)
    }
  }

  async anUnusableHostnameIsRefused() {
    const cookie = await this.server._login()
    const res = await this.server._fetch('/aperio/api/explain?hostname=not%20a%20host!', {
      headers: { cookie },
    })
    assert.equal(res.status, 400)
  }
}

/** The dump carries what it is asked for, and no more. */
export class ExportSpec extends Test({
  // Ordered: this file's specs share one server and change it, so they
  // take turns. Files do not, they have a server each.
  after: () => [ExplainSpec],
  timeout: 60_000,
  dependencies: { server: () => IsolationServer },
}) {
  async theDefaultDumpIsTheConfigurationWithoutTheHistory() {
    const dump = await this.server._api<Record<string, unknown>>('/aperio/api/export')
    assert.ok('tokens' in dump)
    assert.ok(!('statistics' in dump) || dump.statistics === null, 'history is opt-in')
  }

  async includeCarriesTheSectionsItNamesAndNoOthers() {
    const dump = await this.server._api<Record<string, unknown>>(
      '/aperio/api/export?include=statistics,organizations',
    )
    assert.ok(dump.statistics)
    assert.ok(!dump.webhooks, 'a section nobody asked for travelled')
  }

  async withoutTheOrganizationsSectionAChildsRowsStayBehind() {
    const dump = await this.server._api<Record<string, unknown>>('/aperio/api/export?include=tokens')
    assert.ok(
      !JSON.stringify(dump).includes('acme-token'),
      "a child org's token travelled without its organization",
    )
  }

  async aMisspelledSectionIsRefusedRatherThanDropped() {
    const cookie = await this.server._login()
    const res = await this.server._fetch('/aperio/api/export?include=nope', { headers: { cookie } })
    assert.equal(res.status, 400)
  }
}

/** A child org's traffic is its own, in both directions. */
export class TrafficIsolationSpec extends Test({
  // Ordered: this file's specs share one server and change it, so they
  // take turns. Files do not, they have a server each.
  after: () => [ExportSpec],
  timeout: 120_000,
  dependencies: {
    server: () => IsolationServer,
    backend: () => IsolationBackend,
    main: () => IsolationClient,
    orgClient: () => OrgTrafficClient,
  },
}) {
  static orgId = ''

  async _select(id: string) {
    const cookie = await this.server._login()
    const res = await this.server._fetch('/aperio/api/orgs/select', {
      method: 'POST',
      headers: { cookie, 'content-type': 'application/json' },
      body: JSON.stringify({ id }),
    })
    return res.status
  }

  async aChildOrgsRequestIsAbsentFromMastersLog() {
    const org = await this.server._api<{ id: string }>('/aperio/api/orgs', {
      method: 'POST',
      body: JSON.stringify({ name: 'traffico' }),
    })
    TrafficIsolationSpec.orgId = org.id

    await this._select(org.id)
    const token = await this.server._api<{ token: string }>('/aperio/api/tokens', {
      method: 'POST',
      // An org token serving a route has to be able to declare it open now
      // that the server is closed by default, the same deliberate choice a
      // real operator makes when minting one.
      body: JSON.stringify({ name: 'org-client', hostnames: [ORG_HOST], allow_public: true }),
    })
    await this._select('master')

    this.orgClient._token = token.token
    await this.orgClient._start()
    await this.orgClient._waitRoutable(ORG_HOST, '/hello')
    await this.server._fetch('/orgtraffic-probe', { host: ORG_HOST })

    const masterLogs = await this.server._api<{ uri: string }[]>('/aperio/api/logs')
    assert.ok(
      !masterLogs.some((l) => l.uri.includes('orgtraffic-probe')),
      "the child org's traffic leaked into master's log",
    )
  }

  async masterCannotPutAnotherOrgsHostnameIntoMaintenance() {
    const cookie = await this.server._login()
    const res = await this.server._fetch('/aperio/api/maintenance', {
      method: 'POST',
      headers: { cookie, 'content-type': 'application/json' },
      body: JSON.stringify({ hostname: ORG_HOST, enabled: true }),
    })
    assert.equal(res.status, 403)
  }

  async theChildOrgSeesItsOwnTrafficAndCountsIt() {
    await this._select(TrafficIsolationSpec.orgId)
    await waitFor(
      async () => {
        const logs = await this.server._api<{ uri: string }[]>('/aperio/api/logs')
        return logs.some((l) => l.uri.includes('orgtraffic-probe'))
      },
      { label: "the child org's own log" },
    )
    const stats = await this.server._api<{ total_requests: number }>('/aperio/api/stats')
    assert.ok(stats.total_requests >= 1, "the child org's stats count its own request")
    await this._select('master')

    await this.orgClient._kill()
  }
}

/** Cross-org lifecycle: what master may and may not do to a child. */
export class OrgLifecycleSpec extends Test({
  // Ordered: this file's specs share one server and change it, so they
  // take turns. Files do not, they have a server each.
  after: () => [TrafficIsolationSpec],
  timeout: 90_000,
  dependencies: { server: () => IsolationServer },
}) {
  async _status(path: string, method: string, body?: unknown): Promise<number> {
    const cookie = await this.server._login()
    const res = await this.server._fetch(path, {
      method,
      headers: { cookie, 'content-type': 'application/json' },
      body: body === undefined ? undefined : JSON.stringify(body),
    })
    return res.status
  }

  async selectingAnUnknownOrgIs404() {
    assert.equal(await this._status('/aperio/api/orgs/select', 'POST', { id: 'nope' }), 404)
  }

  async aNonEmptyChildOrgCannotBeDeletedAndAnEmptyOneCan() {
    // Makes the org it is about to delete rather than reaching for one
    // another file created: a spec that only passes when its neighbour ran
    // is a spec nobody can run on its own to find out what broke.
    const acme = (
      await this.server._api<{ id: string }>('/aperio/api/orgs', {
        method: 'POST',
        body: JSON.stringify({ name: 'lifecycle' }),
      })
    ).id
    await this._status('/aperio/api/orgs/select', 'POST', { id: acme })
    await this.server._api('/aperio/api/tokens', {
      method: 'POST',
      body: JSON.stringify({ name: 'lifecycle-token', hostnames: ['lifecycle.e2e.local'] }),
    })
    await this._status('/aperio/api/orgs/select', 'POST', { id: 'master' })

    assert.equal(await this._status(`/aperio/api/orgs/${acme}`, 'DELETE'), 409)

    // Empty it from inside, which is also where a cross-org by-id revoke from
    // master is refused: the existence of the row is hidden, so 404.
    await this._status('/aperio/api/orgs/select', 'POST', { id: acme })
    for (const [path, key] of [
      ['/aperio/api/tokens', 'id'],
      ['/aperio/api/webhooks', 'id'],
      ['/aperio/api/users', 'id'],
    ] as const) {
      const rows = await this.server._api<Record<string, string>[]>(path)
      for (const row of rows) {
        await this._status(`${path}/${row[key]}`, 'DELETE')
      }
    }
    await this._status('/aperio/api/orgs/select', 'POST', { id: 'master' })

    assert.equal(await this._status(`/aperio/api/orgs/${acme}`, 'DELETE'), 200)
    assert.equal(await this._status(`/aperio/api/orgs/${acme}`, 'DELETE'), 404)
  }
}
