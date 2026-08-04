import { Test } from 'nole'
import assert from 'node:assert/strict'
import { BaseServer, BaseBackend, BaseClient, HOST } from './fixtures.js'
import { MetricsSpec } from './dashboard.test.js'

interface LogRow {
  id: string
  uri: string
}

interface Timeline {
  dispatched_us: number
  client_received_us: number | null
  backend_sent_us: number | null
  backend_first_byte_us: number | null
  backend_done_us: number | null
  client_responded_us: number | null
  response_received_us: number
  finished_us: number
  estimated_anchor: boolean
}

export class InspectorSpec extends Test({
  // Ordered rather than left to overlap: these specs share one
  // server and change it, so under `--concurrency` they would contend.
  after: () => [MetricsSpec],
  timeout: 90_000,
  dependencies: {
    server: () => BaseServer,
    backend: () => BaseBackend,
    client: () => BaseClient,
  },
}) {
  async _captureOf(prefix: string): Promise<string> {
    const logs = await this.server._api<LogRow[]>('/aperio/api/logs')
    const row = logs.find((l) => l.uri.startsWith(prefix))
    assert.ok(row, `no ${prefix} request in the log`)
    return row.id
  }

  async aCaptureCarriesAnOrderedHighResolutionTimeline() {
    await this.server._fetch('/inspect-me', { host: HOST })
    const id = await this._captureOf('/inspect-me')
    const detail = await this.server._api<{ method: string; uri: string; timeline: Timeline }>(
      `/aperio/api/requests/${id}`,
    )
    assert.equal(detail.method, 'GET')
    assert.match(detail.uri, /\/inspect-me/)

    const tl = detail.timeline
    assert.ok(tl, 'the capture has no timeline')
    const order = [
      0,
      tl.dispatched_us,
      tl.client_received_us,
      tl.backend_sent_us,
      tl.backend_first_byte_us,
      tl.backend_done_us,
      tl.client_responded_us,
      tl.response_received_us,
      tl.finished_us,
    ]
    for (const [i, value] of order.entries()) {
      assert.ok(value !== null && value !== undefined, `stage ${i} is missing`)
      if (i > 0) assert.ok(value! >= order[i - 1]!, `stages out of order: ${order}`)
    }
    assert.equal(tl.estimated_anchor, true)
  }

  async theStageStatisticsAccumulatePerRoute() {
    const routes = await this.server._api<
      { host: string; stages: { stage: string; count: number }[] }[]
    >('/aperio/api/stage-stats')
    const row = routes.find((r) => r.host === HOST)
    assert.ok(row, `no per-route row for ${HOST} in ${JSON.stringify(routes)}`)
    const counts = Object.fromEntries(row.stages.map((s) => [s.stage, s.count]))
    assert.ok(counts.queue > 0, 'no queue samples')
    assert.ok(counts.backend_wait > 0, 'no backend-wait samples')
  }

  async aCaptureCanBeReplayedAndAnUnknownIdIs404() {
    const id = await this._captureOf('/inspect-me')
    const replay = await this.server._api<{ status: number; replayed_id: string }>(
      `/aperio/api/requests/${id}/replay`,
      { method: 'POST' },
    )
    assert.equal(replay.status, 200)
    assert.ok(replay.replayed_id)

    const cookie = await this.server._login()
    const unknown = await this.server._fetch('/aperio/api/requests/no-such-id', {
      headers: { cookie },
    })
    assert.equal(unknown.status, 404)
  }

  async secretsNeverLeaveTheServerButTheCaptureStillReplays() {
    await this.server._fetch('/redact-me', {
      host: HOST,
      method: 'POST',
      headers: {
        authorization: 'Bearer sk-live-e2e-token',
        cookie: 'sid=e2e-cookie-secret',
        'content-type': 'application/json',
      },
      body: JSON.stringify({ username: 'doga', password: 'e2e-hunter2' }),
    })
    const id = await this._captureOf('/redact-me')
    const raw = await this.server._fetch(`/aperio/api/requests/${id}`, {
      headers: { cookie: await this.server._login() },
    })
    assert.match(raw.body, /Bearer \[REDACTED\]/)
    assert.doesNotMatch(raw.body, /sk-live-e2e-token/, 'the bearer token leaked')
    assert.doesNotMatch(raw.body, /e2e-cookie-secret/, 'the cookie value leaked')

    const detail = JSON.parse(raw.body) as { req_body: string }
    const body = Buffer.from(detail.req_body, 'base64').toString()
    assert.match(body, /"username":"doga"/, 'non-secret fields stay readable')
    assert.match(body, /"password":"\[REDACTED\]"/)
    assert.doesNotMatch(body, /e2e-hunter2/, 'the password leaked into the captured body')

    // The raw capture is intact server-side, so a replay still carries the
    // original bytes.
    const replay = await this.server._api<{ status: number }>(
      `/aperio/api/requests/${id}/replay`,
      { method: 'POST' },
    )
    assert.equal(replay.status, 200)
  }

  async passkeysAreOffUntilAnOriginIsConfigured() {
    const res = await this.server._fetch('/aperio/auth/passkey/discoverable/start', {
      method: 'POST',
    })
    assert.equal(res.status, 501)
  }
}
