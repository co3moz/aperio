import { Test } from 'nole'
import assert from 'node:assert/strict'
import { mkdtemp, readFile } from 'node:fs/promises'
import { existsSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { waitFor, sleep } from '../../lib/env.js'
import { send } from '../../lib/http.js'
import { SseStream } from '../../lib/sse.js'
import { MqttClient } from '../../lib/mqtt.js'
import {
  MessageServer,
  MessageBackend,
  SubscriberClient,
  ScopedClient,
  MqttClientA,
  MqttClientB,
  RunnerClient,
} from './fixtures.js'

/** One publish, one delivery per client *process*, however many connections
 *  that process holds. */
export class PublishDeliverySpec extends Test({
  timeout: 120_000,
  dependencies: {
    server: () => MessageServer,
    backend: () => MessageBackend,
    subscriber: () => SubscriberClient,
  },
}) {
  _sse!: SseStream

  async before() {
    await this.subscriber._start()
    await waitFor(async () => (await send(this.subscriber._faceUrl(), '/')).status < 500, {
      label: 'the message face to come up',
    })
    this._sse = await SseStream.open(this.subscriber._faceUrl(), 'deploy/#')
    await sleep(500)
  }

  async aPublishReachesTheProcessExactlyOnce() {
    const published = await this.server._api<{ clients: number }>('/aperio/api/publish', {
      method: 'POST',
      body: JSON.stringify({ topic: 'deploy/web', payload: 'ship-it' }),
    })
    assert.equal(published.clients, 1, 'two connections, one process, one delivery')

    await waitFor(() => this._sse.count('deploy/web') === 1, { label: 'the delivery' })
    // A payload is bytes and an SSE field is a line, so it travels base64.
    assert.equal(this._sse.first('deploy/web')?.data, Buffer.from('ship-it').toString('base64'))
  }

  async aTopicNobodyAskedForIsNotDeliveredAndIsNotAnError() {
    await this.server._api('/aperio/api/publish', {
      method: 'POST',
      body: JSON.stringify({ topic: 'metrics/cpu', payload: '99' }),
    })
    await sleep(1_000)
    assert.equal(this._sse.count('metrics/cpu'), 0)
  }

  async theLocalFacePublishesOverTheClientsOwnTunnel() {
    // No admin credential: the client's own token carries it.
    const res = await send(this.subscriber._faceUrl(), '/publish?topic=deploy%2Ffrom-face', {
      method: 'POST',
      body: 'local',
    })
    assert.equal(res.status, 202)
    await waitFor(() => this._sse.count('deploy/from-face') === 1, { label: 'the round trip' })
  }

  async aClientMayNotPublishIntoTheServersOwnNamespace() {
    const res = await send(this.subscriber._faceUrl(), '/publish?topic=%24aperio%2Fforged', {
      method: 'POST',
      body: 'x',
    })
    assert.equal(res.status, 400)
  }

  async serverEventsArriveOnTheAperioNamespace() {
    const events = await SseStream.open(this.subscriber._faceUrl(), '$aperio/#')
    await sleep(500)
    await this.server._mintToken({ name: 'e2e-message-token' })
    await waitFor(() => events.count('$aperio/token/created') > 0, {
      label: 'the server event',
    })
    events.close()
  }

  async aQos1MessageIsAcknowledgedSoItArrivesOnceAndStops() {
    const published = await this.server._api<{ qos: number }>('/aperio/api/publish', {
      method: 'POST',
      body: JSON.stringify({ topic: 'deploy/once', payload: 'exactly', qos: 1 }),
    })
    assert.equal(published.qos, 1, 'the publish reports the qos it was accepted at')

    await waitFor(() => this._sse.count('deploy/once') === 1, { label: 'the qos 1 delivery' })
    // Well past two retry timeouts: a missing acknowledgement would have
    // produced more copies by now.
    await sleep(8_000)
    assert.equal(this._sse.count('deploy/once'), 1)
  }

  async after() {
    this._sse?.close()
  }
}

/** A token is fenced to the topics it carries, in both directions. */
export class ScopedTopicSpec extends Test({
  timeout: 120_000,
  after: () => [PublishDeliverySpec],
  dependencies: {
    server: () => MessageServer,
    backend: () => MessageBackend,
    scoped: () => ScopedClient,
  },
}) {
  _sse!: SseStream

  async before() {
    const minted = await this.server._mintToken({
      name: 'e2e-scoped',
      hostnames: ['*'],
      paths: ['*'],
      topics: ['deploy/#'],
    })
    this.scoped._token = minted.token
    await this.scoped._start()
    await waitFor(async () => (await send(this.scoped._faceUrl(), '/')).status < 500, {
      label: "the scoped client's face",
    })
    this._sse = await SseStream.open(this.scoped._faceUrl(), 'deploy/#')
    await sleep(500)
  }

  async itReceivesWhatItCarriesAndNotWhatItDoesNot() {
    for (const [topic, payload] of [
      ['deploy/scoped', 'yes'],
      ['secrets/rotate', 'no'],
    ]) {
      await this.server._api('/aperio/api/publish', {
        method: 'POST',
        body: JSON.stringify({ topic, payload }),
      })
    }
    await waitFor(() => this._sse.count('deploy/scoped') > 0, { label: 'the in-scope topic' })
    assert.equal(this._sse.count('secrets/rotate'), 0, 'a topic outside the scope arrived')
    // Named, not silent: the client is told which filter was refused.
    await this.scoped._waitForLog("Not subscribed to 'secrets/#'")
  }

  async aRefusedPublishIsReportedToThePublishingSide() {
    // The face answers 202 the moment the message is on the tunnel, so a
    // refusal that only reached the server's log would leave the publisher
    // believing it had sent something.
    await send(this.scoped._faceUrl(), '/publish?topic=secrets%2Frotate', {
      method: 'POST',
      body: 'no',
    })
    await this.scoped._waitForLog("published on 'secrets/rotate' went nowhere")
  }

  async after() {
    this._sse?.close()
  }
}

/** An ordinary MQTT client on one machine, another on the next. */
export class MqttFaceSpec extends Test({
  timeout: 120_000,
  after: () => [PublishDeliverySpec],
  dependencies: {
    server: () => MessageServer,
    backend: () => MessageBackend,
    a: () => MqttClientA,
    b: () => MqttClientB,
  },
}) {
  async anMqttPublishOnOneMachineReachesASubscriberOnAnother() {
    await this.a._start()
    await this.b._start()

    const subscriber = await MqttClient.connect(this.a._facePort, 'e2e-sub')
    subscriber.subscribe('deploy/#')
    await waitFor(() => subscriber.suback, { label: 'the SUBSCRIBE to be answered' })

    // Published from the *other* machine's face, so the message crosses the
    // server rather than staying inside one process.
    const publisher = await MqttClient.connect(this.b._facePort, 'e2e-pub')
    await sleep(500)
    publisher.publish('deploy/mqtt', 'over-the-tunnel')

    await waitFor(
      () =>
        subscriber.messages.some(
          (m) => m.topic === 'deploy/mqtt' && m.payload === 'over-the-tunnel',
        ),
      { label: 'the message to cross' },
    )
    subscriber.close()
    publisher.close()
  }
}

/** A subscription may run a command, and the payload must stay data. */
export class SubscriptionRunSpec extends Test({
  timeout: 120_000,
  after: () => [PublishDeliverySpec],
  dependencies: {
    server: () => MessageServer,
    backend: () => MessageBackend,
    runner: () => RunnerClient,
  },
}) {
  async thePayloadReachesTheCommandOnStdinAndIsNeverInterpreted() {
    const dir = await mkdtemp(join(tmpdir(), 'aperio-run-'))
    this.runner._runDir = dir
    await this.runner._start()
    await this.runner._waitRoutable('msgrun.e2e.local', '/hello')

    // A payload built to break out of a shell command.
    const hostile = `'; touch ${join(dir, 'PWNED')} ; echo '`
    await this.server._api('/aperio/api/publish', {
      method: 'POST',
      body: JSON.stringify({ topic: 'deploy/run', payload: hostile }),
    })

    await waitFor(() => existsSync(join(dir, 'payload')), { label: 'the command to run' })
    assert.equal(
      await readFile(join(dir, 'topic'), 'utf8'),
      'deploy/run',
      'the topic reaches the command through the environment',
    )
    assert.equal(
      await readFile(join(dir, 'payload'), 'utf8'),
      hostile,
      'the payload arrives on stdin byte for byte',
    )
    assert.ok(!existsSync(join(dir, 'PWNED')), 'the payload was interpreted by the shell')
  }
}

/** Prometheus rejects a whole scrape over one missing TYPE line. */
export class MessageMetricsSpec extends Test({
  timeout: 60_000,
  after: () => [PublishDeliverySpec, ScopedTopicSpec],
  dependencies: { server: () => MessageServer, subscriber: () => SubscriberClient },
}) {
  async theMessagingCountersAreScrapeable() {
    const res = await this.server._fetch('/aperio/metrics?token=e2e-scrape')
    assert.equal(res.status, 200)
    for (const family of [
      'aperio_messages_published_total',
      'aperio_messages_delivered_total',
      'aperio_messages_dropped_total',
      'aperio_message_subscribers',
      'aperio_messages_awaiting_ack',
    ]) {
      assert.match(res.body, new RegExp(`^# TYPE ${family} `, 'm'), `${family} has no TYPE line`)
      assert.match(res.body, new RegExp(`^${family} `, 'm'), `${family} has no sample`)
    }

    const value = (family: string) =>
      Number(new RegExp(`^${family} (\\S+)`, 'm').exec(res.body)?.[1] ?? '0')
    assert.ok(value('aperio_messages_published_total') > 0, 'nothing counted as published')
    assert.ok(value('aperio_messages_delivered_total') > 0, 'nothing counted as delivered')
  }
}
