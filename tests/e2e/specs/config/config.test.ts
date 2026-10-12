import { Test } from 'nole'
import assert from 'node:assert/strict'
import { mkdtemp, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { execFile } from 'node:child_process'
import { promisify } from 'node:util'
import { AperioServerBase } from '../../lib/server.js'
import { StandardBackendBase } from '../../lib/backend.js'
import { ClientFor } from '../../lib/client.js'
import { SERVER_BIN, waitFor } from '../../lib/env.js'

const run = promisify(execFile)

/** Runs the server binary with a command, or with none and a budget: a
 *  start that should have been refused is killed and reported, rather than
 *  left to hang the phase. */
async function serverCli(args: string[], env: Record<string, string> = {}) {
  try {
    const { stdout, stderr } = await run(SERVER_BIN, args, {
      env: { ...process.env, ...env },
      maxBuffer: 16 * 1024 * 1024,
      timeout: 15_000,
    })
    return { ok: true, out: stdout + stderr }
  } catch (e) {
    const err = e as { stdout?: string; stderr?: string }
    return { ok: false, out: (err.stdout ?? '') + (err.stderr ?? '') }
  }
}

const PROBE = (body: string) => [
  { path: '/reload-probe', respond: { status: 200, body } },
]

export class SettingsServer extends AperioServerBase({
  settings: {
    cache: { enabled: false },
    security: { login_lockout: { threshold: 5 } },
    routes: PROBE('v1'),
  },
}) {}

interface SettingsDoc {
  settings: {
    cache: { enabled: boolean }
    security: { login_lockout: { threshold: number } }
    metrics: { enabled: boolean }
  }
  restart_pending: string[]
}

export class ServerSettingsSpec extends Test({
  timeout: 120_000,
  dependencies: { server: () => SettingsServer },
}) {
  async _doc(): Promise<SettingsDoc> {
    return this.server._api<SettingsDoc>('/aperio/api/settings')
  }

  async theFirstStartDocumentIsInEffect() {
    const doc = await this._doc()
    assert.equal(doc.settings.cache.enabled, false)
    assert.equal(doc.settings.security.login_lockout.threshold, 5)
    const res = await this.server._fetch('/reload-probe', { host: 'probe.e2e.local' })
    assert.equal(res.body, 'v1', 'the client-less route serves its initial body')
  }

  async aChangeAppliesLiveWithoutARestart() {
    const res = await this.server._putSettings({
      cache: { enabled: true },
      security: { login_lockout: { threshold: 9 } },
      routes: PROBE('v2-changed'),
    })
    assert.equal(res.status, 200, res.body)
    const doc = await this._doc()
    assert.equal(doc.settings.cache.enabled, true)
    assert.equal(doc.settings.security.login_lockout.threshold, 9)
    const probe = await this.server._fetch('/reload-probe', { host: 'probe.e2e.local' })
    assert.equal(probe.body, 'v2-changed')
  }

  async theFirstStartDocumentIsNotReadAgain() {
    // The settings live in the database from here on: a restart reads them
    // there, and the document that seeded them changes nothing.
    await this.server._restart()
    const doc = await this._doc()
    assert.equal(doc.settings.security.login_lockout.threshold, 9)
  }

  async aDeniedAddressIsRefusedServerWideAndTheResetLetsYouBackIn() {
    let res = await this.server._putSettings({ security: { denied_ips: ['203.0.113.7'] } })
    assert.equal(res.status, 200, res.body)
    assert.equal(
      (await this.server._fetch('/aperio/health')).status,
      200,
      'denying an unrelated address leaves everybody else alone',
    )

    res = await this.server._putSettings({ security: { denied_ips: ['127.0.0.1', '::1'] } })
    assert.equal(res.status, 200, res.body)
    assert.equal((await this.server._fetch('/aperio/health')).status, 403)
    // Server-wide, not just the proxy path.
    const cookie = await this.server._login()
    assert.equal((await this.server._fetch('/aperio/api/stats', { headers: { cookie } })).status, 403)
    assert.equal(
      (await this.server._fetch('/reload-probe', { host: 'probe.e2e.local' })).status,
      403,
      'the deny list is checked before a client-less route answers',
    )

    // Locked out of the API by its own setting: the way back in is the
    // command on the stopped server's data directory.
    await this.server._stop()
    const reset = await serverCli(['settings', 'reset', 'security.denied_ips'], {
      APERIO_DATA_DIR: this.server._dataDir,
    })
    assert.ok(reset.ok, reset.out)
    await this.server._spawn()
    assert.equal((await this.server._fetch('/aperio/health')).status, 200)
    const probe = await this.server._fetch('/reload-probe', { host: 'probe.e2e.local' })
    assert.equal(probe.body, 'v2-changed', 'the reset put back that one setting and no other')
  }

  async aStartupSettingIsStoredAndPendingUntilTheNextStart() {
    const res = await this.server._putSettings({ metrics: { enabled: true } })
    assert.equal(res.status, 200, res.body)
    assert.ok((await this._doc()).restart_pending.includes('metrics.enabled'))
    await this.server._restart()
    const doc = await this._doc()
    assert.equal(doc.settings.metrics.enabled, true)
    assert.deepEqual(doc.restart_pending, [], 'the restart put it in force')
  }

  async theEdgeEndpointsOwnTheirPathsEvenWithTheFeatureOff() {
    // Registered only with the token, these fell through to the visitor proxy
    // and came back as a 504 "no client connected", which reads as a tunnel
    // fault rather than a feature that is off.
    for (const path of ['edge/traefik', 'edge/ask?domain=probe.e2e.local']) {
      const res = await this.server._fetch(`/aperio/api/${path}`)
      assert.equal(res.status, 404, path)
      assert.match(res.body, /edge integration is not enabled/, path)
    }
  }

  async aPerHostnameErrorPageIsServedOnceSaved() {
    const page = join(this.server._dataDir, 'custom-504.html')
    await writeFile(page, '<h1>custom err.e2e.local 504</h1>\n')
    const res = await this.server._putSettings({
      error_pages: [{ hostname: 'err.e2e.local', '504_page': page }],
    })
    assert.equal(res.status, 200, res.body)
    await waitFor(
      async () =>
        (await this.server._fetch('/nothing', { host: 'err.e2e.local' })).body.includes(
          'custom err.e2e.local 504',
        ),
      { label: 'the per-hostname 504 page' },
    )
    const other = await this.server._fetch('/nothing', { host: 'other.e2e.local' })
    assert.match(other.body, /504 Gateway Timeout/, 'other hostnames keep the default text')
  }
}

/** The commands that never start a server. */
export class ServerCliSpec extends Test({ timeout: 90_000 }) {
  async _tmpDocument(yaml: string): Promise<string> {
    const dir = await mkdtemp(join(tmpdir(), 'aperio-cfg-'))
    const path = join(dir, 'settings.yaml')
    await writeFile(path, yaml)
    return path
  }

  async theSchemaIsValidJson() {
    const res = await serverCli(['schema'])
    assert.ok(res.ok, res.out)
    const schema = JSON.parse(res.out) as { properties: Record<string, unknown> }
    assert.ok(schema.properties.limits, 'the settings sections are described')
  }

  async checkPassesAValidDocumentAndFailsAnInvalidOne() {
    const good = await this._tmpDocument('settings:\n  routing:\n    lb_strategy: sticky\n')
    const ok = await serverCli(['check', good])
    assert.ok(ok.ok, ok.out)
    assert.match(ok.out, /is valid \(settings\)/)

    for (const bad of [
      'settings:\n  routing:\n    lb_strategy: bogus\n',
      'settings:\n  limits:\n    request_body: lots\n',
      'settings:\n  proxy:\n    trusted: [not-an-address]\n',
      'settngs:\n  cache:\n    enabled: true\n',
    ]) {
      const path = await this._tmpDocument(bad)
      const fails = await serverCli(['check', path])
      assert.equal(fails.ok, false, `${bad} must be refused`)
    }
  }

  async applyStoresSettingsAndResetPutsThemBack() {
    const data = await mkdtemp(join(tmpdir(), 'aperio-apply-'))
    const env = { APERIO_DATA_DIR: data }
    const doc = await this._tmpDocument('settings:\n  routing:\n    lb_strategy: sticky\n')
    const applied = await serverCli(['apply', doc], env)
    assert.ok(applied.ok, applied.out)
    assert.match((await serverCli(['settings', 'show'], env)).out, /lb_strategy: sticky/)

    const reset = await serverCli(['settings', 'reset', 'routing.lb_strategy'], env)
    assert.ok(reset.ok, reset.out)
    assert.doesNotMatch((await serverCli(['settings', 'show'], env)).out, /sticky/)
  }

  async aMovedVariableRefusesTheStartAndNamesWhereItWent() {
    const data = await mkdtemp(join(tmpdir(), 'aperio-moved-'))
    const res = await serverCli([], {
      APERIO_SERVER_TOKEN: 'e2e-moved-token-long-enough',
      APERIO_DATA_DIR: data,
      APERIO_TRUSTED_PROXIES: '10.0.0.0/8',
    })
    assert.equal(res.ok, false, 'the server must refuse, not run')
    assert.match(res.out, /APERIO_TRUSTED_PROXIES/)
    assert.match(res.out, /proxy\.trusted/, 'and say which setting replaced it')
  }

  async theFlagsTheCommandsReplacedAreRefused() {
    for (const flag of ['--check-config', '--print-config', '--print-schema']) {
      assert.equal((await serverCli([flag])).ok, false, flag)
    }
  }
}

/** Binary frames bypass the tunnel's text compression, so the frame deflates
 *  its own payload; what has to hold is that the bytes still come back. */
export class CompressedTunnelServer extends AperioServerBase({
  settings: { tunnel: { compression: true } },
}) {}

export class CompressedBackend extends StandardBackendBase() {}

export class CompressedClient extends ClientFor(() => CompressedTunnelServer, () => CompressedBackend) {
  _hostname() {
    return 'comp.e2e.local'
  }
  _readyPath() {
    return '/hello'
  }
  _env() {
    return { APERIO_HOSTNAME: 'comp.e2e.local' }
  }
}

export class TunnelCompressionSpec extends Test({
  timeout: 90_000,
  dependencies: {
    server: () => CompressedTunnelServer,
    backend: () => CompressedBackend,
    client: () => CompressedClient,
  },
}) {
  async everyByteValueSurvivesADownload() {
    const want = Buffer.from([...Array(256).keys(), ...Array(256).keys()])
    const res = await this.server._fetch('/binary', { host: 'comp.e2e.local' })
    assert.deepEqual(res.bytes, want)
  }

  async aCompressibleUploadSurvivesTheOtherWay() {
    const want = Buffer.concat([
      Buffer.from([...Array(256).keys(), ...Array(256).keys()]),
      Buffer.from('field=value&'.repeat(400)),
    ])
    const res = await this.server._fetch('/echo-body', {
      host: 'comp.e2e.local',
      method: 'POST',
      headers: { 'content-type': 'application/octet-stream' },
      body: want,
    })
    assert.deepEqual(res.bytes, want)
  }
}
