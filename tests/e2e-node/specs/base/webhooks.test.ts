import { Test } from 'nole'
import assert from 'node:assert/strict'
import { waitFor } from '../../lib/env.js'
import { BaseServer, BaseBackend, BaseClient, EDGE_TOKEN, HOST } from './fixtures.js'
import { InspectorSpec } from './inspector.test.js'

interface Webhook {
  id: string
  name: string
  format?: string
}

interface Delivery {
  id: string
  webhook_name: string
  success: boolean
  attempts: number
  event?: string
}

export class WebhooksApiSpec extends Test({
  // Ordered rather than left to overlap: these specs share one
  // server and change it, so under `--concurrency` they would contend.
  after: () => [InspectorSpec],
  timeout: 90_000,
  dependencies: { server: () => BaseServer, backend: () => BaseBackend },
}) {
  async _create(body: Record<string, unknown>) {
    return this.server._api<Webhook>('/aperio/api/webhooks', {
      method: 'POST',
      body: JSON.stringify(body),
    })
  }

  async _delete(id: string): Promise<number> {
    const cookie = await this.server._login()
    return (
      await this.server._fetch(`/aperio/api/webhooks/${id}`, { method: 'DELETE', headers: { cookie } })
    ).status
  }

  async aHookIsCreatedListedAndDeletedExactlyOnce() {
    const hook = await this._create({
      name: 'e2e-hook',
      url: `${this.backend._url}/hook`,
      events: ['client_connected'],
    })
    const list = await this.server._api<Webhook[]>('/aperio/api/webhooks')
    assert.ok(list.some((h) => h.name === 'e2e-hook'))

    assert.equal(await this._delete(hook.id), 200)
    assert.equal(await this._delete(hook.id), 404, 'deleting twice is a 404')
  }

  async aNonHttpUrlIsRejected() {
    const cookie = await this.server._login()
    const res = await this.server._fetch('/aperio/api/webhooks', {
      method: 'POST',
      headers: { cookie, 'content-type': 'application/json' },
      body: JSON.stringify({ name: 'bad', url: 'ftp://nope' }),
    })
    assert.equal(res.status, 400)
  }

  async aTestFireReportsTheOutcomeRatherThanOnlyLoggingIt() {
    const slack = await this._create({
      name: 'e2e-slack',
      url: `${this.backend._url}/hook`,
      events: ['*'],
      format: 'slack',
    })
    const list = await this.server._api<Webhook[]>('/aperio/api/webhooks')
    assert.equal(list.find((h) => h.id === slack.id)?.format, 'slack')

    const fired = await this.server._api<{ ok: boolean; status: number }>(
      `/aperio/api/webhooks/${slack.id}/test`,
      { method: 'POST' },
    )
    assert.equal(fired.ok, true)
    assert.equal(fired.status, 200)

    const deliveries = await this.server._api<Delivery[]>('/aperio/api/webhooks/deliveries')
    assert.ok(
      deliveries.some((d) => JSON.stringify(d).includes('webhook_test')),
      'the test delivery is recorded',
    )

    const cookie = await this.server._login()
    const unknown = await this.server._fetch('/aperio/api/webhooks/nope/test', {
      method: 'POST',
      headers: { cookie },
    })
    assert.equal(unknown.status, 404)

    const badFormat = await this.server._fetch('/aperio/api/webhooks', {
      method: 'POST',
      headers: { cookie, 'content-type': 'application/json' },
      body: JSON.stringify({ name: 'bad-format', url: 'http://127.0.0.1:1/x', format: 'telegram' }),
    })
    assert.equal(badFormat.status, 400)

    assert.equal(await this._delete(slack.id), 200)
  }
}

export class WebhookDeliverySpec extends Test({
  // Ordered rather than left to overlap: these specs share one
  // server and change it, so under `--concurrency` they would contend.
  after: () => [WebhooksApiSpec],
  timeout: 120_000,
  dependencies: { server: () => BaseServer, backend: () => BaseBackend },
}) {
  static goodId = ''

  async aReachableHookSucceedsAndAnUnreachableOneIsRetriedThenFailed() {
    const good = await this.server._api<Webhook>('/aperio/api/webhooks', {
      method: 'POST',
      body: JSON.stringify({
        name: 'e2e-deliveries',
        url: `${this.backend._url}/hook`,
        events: ['token_created'],
      }),
    })
    WebhookDeliverySpec.goodId = good.id
    await this.server._api('/aperio/api/webhooks', {
      method: 'POST',
      body: JSON.stringify({
        name: 'e2e-dead',
        url: 'http://127.0.0.1:9/hook',
        events: ['token_created'],
      }),
    })

    // The event both hooks are waiting for.
    await this.server._mintToken({ name: 'dlv-probe', hostnames: ['dlv.e2e.local'] })

    let deliveries: Delivery[] = []
    await waitFor(
      async () => {
        deliveries = await this.server._api<Delivery[]>('/aperio/api/webhooks/deliveries')
        const names = new Set(deliveries.map((d) => d.webhook_name))
        return names.has('e2e-deliveries') && names.has('e2e-dead')
      },
      { timeoutMs: 30_000, label: 'both deliveries to be recorded' },
    )

    const succeeded = deliveries.find((d) => d.webhook_name === 'e2e-deliveries')
    assert.equal(succeeded?.success, true)
    const failed = deliveries.find((d) => d.webhook_name === 'e2e-dead')
    assert.equal(failed?.success, false)
    assert.equal(failed?.attempts, 2, 'the failure was retried per the schedule')
  }

  async aRedeliveryLandsInTheLogAsANewRow() {
    const deliveries = await this.server._api<Delivery[]>('/aperio/api/webhooks/deliveries')
    const row = deliveries.find((d) => d.webhook_name === 'e2e-deliveries')
    assert.ok(row)

    const cookie = await this.server._login()
    const accepted = await this.server._fetch(
      `/aperio/api/webhooks/deliveries/${row.id}/redeliver`,
      { method: 'POST', headers: { cookie } },
    )
    assert.equal(accepted.status, 202)

    await waitFor(
      async () => {
        const rows = await this.server._api<Delivery[]>(
          `/aperio/api/webhooks/deliveries?webhook_id=${WebhookDeliverySpec.goodId}`,
        )
        return rows.filter((d) => d.webhook_name === 'e2e-deliveries').length >= 2
      },
      { timeoutMs: 20_000, label: 'the redelivery to be logged' },
    )

    const unknown = await this.server._fetch(
      '/aperio/api/webhooks/deliveries/no-such-id/redeliver',
      { method: 'POST', headers: { cookie } },
    )
    assert.equal(unknown.status, 404)
  }
}

/** Caddy's on-demand TLS contract and Traefik's provider document. */
export class EdgeIntegrationSpec extends Test({
  // Ordered rather than left to overlap: these specs share one
  // server and change it, so under `--concurrency` they would contend.
  after: () => [WebhookDeliverySpec],
  timeout: 60_000,
  dependencies: {
    server: () => BaseServer,
    backend: () => BaseBackend,
    client: () => BaseClient,
  },
}) {
  async askAuthorizesOnlyHostnamesSomebodyServes() {
    const auth = { authorization: `Bearer ${EDGE_TOKEN}` }
    const served = await this.server._fetch(`/aperio/api/edge/ask?domain=${HOST}`, { headers: auth })
    assert.equal(served.status, 200, '200 means: issue a certificate')

    const nobody = await this.server._fetch('/aperio/api/edge/ask?domain=nobody.e2e.local', {
      headers: auth,
    })
    assert.equal(nobody.status, 404)

    // Caddy cannot send headers, so the token is accepted in the query.
    const viaQuery = await this.server._fetch(
      `/aperio/api/edge/ask?token=${EDGE_TOKEN}&domain=${HOST}`,
    )
    assert.equal(viaQuery.status, 200)

    const noCredential = await this.server._fetch(`/aperio/api/edge/ask?domain=${HOST}`)
    assert.equal(noCredential.status, 401)
  }

  async theTraefikDocumentRoutesEveryServedHostnameAndIsStable() {
    const auth = { authorization: `Bearer ${EDGE_TOKEN}` }
    const first = await this.server._fetch('/aperio/api/edge/traefik', { headers: auth })
    assert.equal(first.status, 200)
    assert.match(first.body, new RegExp(`Host\\(\`${HOST}\`\\)`))
    assert.match(first.body, /"passHostHeader":true/)
    assert.match(first.body, /"certResolver":"letsencrypt"/)
    assert.match(first.body, /"url":"http:\/\/aperio:8080"/)

    // Byte-identical between polls, or Traefik churns its configuration.
    const second = await this.server._fetch('/aperio/api/edge/traefik', { headers: auth })
    assert.equal(second.body, first.body)
  }
}
