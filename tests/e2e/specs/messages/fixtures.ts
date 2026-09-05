import { AperioServerBase } from '../../lib/server.js'
import { StandardBackendBase } from '../../lib/backend.js'
import { ClientFor } from '../../lib/client.js'
import { freePort } from '../../lib/env.js'

export class MessageServer extends AperioServerBase({
  env: { APERIO_METRICS: '1', APERIO_METRICS_TOKEN: 'e2e-scrape' },
}) {}

export class MessageBackend extends StandardBackendBase() {}

class MessageClient extends ClientFor(() => MessageServer, () => MessageBackend) {
  _facePort = 0
  _faceUrl(): string {
    return `http://127.0.0.1:${this._facePort}`
  }
  async _start() {
    if (!this._facePort) this._facePort = await freePort()
    await super._start()
  }
}

/**
 * Two connections for one process, so the per-process delivery rule is under
 * test rather than assumed: keyed on the connection, this subscriber would
 * receive every message twice.
 */
export class SubscriberClient extends MessageClient {
  _autoStart() {
    return false
  }
  _hostname() {
    return 'msgsub.e2e.local'
  }
  _readyPath() {
    return '/hello'
  }
  _env() {
    return {
      APERIO_CONNECTIONS: '2',
      APERIO_HOSTNAME: 'msgsub.e2e.local',
      APERIO_SUBSCRIBE: 'deploy/#',
      APERIO_MESSAGES_LISTEN: `127.0.0.1:${this._facePort}`,
    }
  }
}

/** A token that carries only `deploy/#`, asking for one more than that. */
export class ScopedClient extends MessageClient {
  _token = ''
  _autoStart() {
    return false
  }
  _serverToken() {
    return this._token
  }
  _hostname() {
    return 'msgscoped.e2e.local'
  }
  _readyPath() {
    return '/hello'
  }
  _env() {
    return {
      APERIO_HOSTNAME: 'msgscoped.e2e.local',
      APERIO_SUBSCRIBE: 'deploy/#,secrets/#',
      APERIO_MESSAGES_LISTEN: `127.0.0.1:${this._facePort}`,
    }
  }
}

/**
 * A token carrying `#`, the "all topics" grant, asking for the server's own
 * namespace on top of an ordinary topic. The wildcard is not supposed to reach
 * it: the infrastructure feed is granted by name or not at all.
 */
export class WildcardTokenClient extends MessageClient {
  _token = ''
  _autoStart() {
    return false
  }
  _serverToken() {
    return this._token
  }
  _hostname() {
    return 'msgwild.e2e.local'
  }
  _readyPath() {
    return '/hello'
  }
  _env() {
    return {
      APERIO_HOSTNAME: 'msgwild.e2e.local',
      APERIO_SUBSCRIBE: 'deploy/#,$aperio/#',
      APERIO_MESSAGES_LISTEN: `127.0.0.1:${this._facePort}`,
    }
  }
}

class MqttFaceClient extends MessageClient {
  _autoStart() {
    return false
  }
  _readyPath() {
    return '/hello'
  }
}

export class MqttClientA extends MqttFaceClient {
  _hostname() {
    return 'mqtta.e2e.local'
  }
  _env() {
    return {
      APERIO_HOSTNAME: 'mqtta.e2e.local',
      APERIO_MESSAGES_MQTT_LISTEN: `127.0.0.1:${this._facePort}`,
    }
  }
}

export class MqttClientB extends MqttFaceClient {
  _hostname() {
    return 'mqttb.e2e.local'
  }
  _env() {
    return {
      APERIO_HOSTNAME: 'mqttb.e2e.local',
      APERIO_MESSAGES_MQTT_LISTEN: `127.0.0.1:${this._facePort}`,
    }
  }
}

/** Runs a command for every message on its topic. */
export class RunnerClient extends MessageClient {
  _runDir = ''

  _autoStart() {
    return false
  }
  _cwd() {
    return this._runDir
  }
  _hostname() {
    return 'msgrun.e2e.local'
  }
  _readyPath() {
    return '/hello'
  }
  _config() {
    return [
      'server:',
      `  url: ${this.server._url}`,
      `  token: ${this.server._token}`,
      'services:',
      `  - target: ${this.backend._url}`,
      '    hostname: msgrun.e2e.local',
      'subscribe:',
      '  - topic: deploy/run',
      // A script the spec writes into the run directory, run by the node
      // this suite already runs on. The runner hands the line to `sh -c` on
      // unix and `cmd /C` on Windows, and `cat`/`printf` only exist for the
      // first, so the command is the one program both shells can start. The
      // path is relative to the process's working directory, which `_cwd`
      // points at the run directory, so it needs no quoting: a quoted path
      // survives both shells but not every `node` on a PATH (a version
      // manager's shim has been seen handing the quotes on to node).
      "    run: 'node run.mjs'",
      '    timeout: 10',
      '',
    ].join('\n')
  }
}
