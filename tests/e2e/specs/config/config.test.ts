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

/** Runs the server binary in one of its report-and-exit modes. */
async function serverCli(args: string[], env: Record<string, string> = {}) {
  try {
    const { stdout, stderr } = await run(SERVER_BIN, args, {
      env: { ...process.env, ...env },
      maxBuffer: 16 * 1024 * 1024,
    })
    return { ok: true, out: stdout + stderr }
  } catch (e) {
    const err = e as { stdout?: string; stderr?: string }
    return { ok: false, out: (err.stdout ?? '') + (err.stderr ?? '') }
  }
}

export class ReloadServer extends AperioServerBase() {
  _configFile() {
    // The grouped form of both settings; the reload below uses the flat
    // spelling, so each of them is covered reaching a running server.
    return [
      'cache:',
      '  enabled: false',
      'login_lockout:',
      '  threshold: 5',
      'routes:',
      '  - path: /reload-probe',
      '    respond:',
      '      status: 200',
      '      body: "v1"',
      '',
    ].join('\n')
  }
}

interface Settings {
  effective: Record<string, unknown>
  file_keys: string[]
}

export class ServerConfigReloadSpec extends Test({
  timeout: 120_000,
  dependencies: { server: () => ReloadServer },
}) {
  async _setting(key: string): Promise<unknown> {
    const settings = await this.server._api<Settings>('/aperio/api/settings')
    return settings.effective[key]
  }

  async theInitialFileValuesAreInEffect() {
    assert.equal(await this._setting('cache_enabled'), false, 'a grouped cache.enabled applies')
    assert.equal(await this._setting('login_lockout_threshold'), 5)
    const res = await this.server._fetch('/reload-probe', { host: 'probe.e2e.local' })
    assert.equal(res.body, 'v1', 'the client-less route serves its initial body')
  }

  async anEditIsAppliedLiveExceptForStructuralKeys() {
    await this.server._writeConfig(
      [
        'cache: true',
        'login_lockout_threshold: 9',
        'port: 9999',
        'routes:',
        '  - path: /reload-probe',
        '    respond:',
        '      status: 200',
        '      body: "v2-reloaded"',
        '',
      ].join('\n'),
    )
    await waitFor(async () => (await this._setting('cache_enabled')) === true, {
      label: 'the edited config to be hot-reloaded',
    })
    assert.equal(await this._setting('login_lockout_threshold'), 9, 'the flat spelling reloads too')

    const res = await this.server._fetch('/reload-probe', { host: 'probe.e2e.local' })
    assert.equal(res.body, 'v2-reloaded')

    // The port change is structural: the server stays where it was.
    const health = await this.server._fetch('/aperio/health')
    assert.equal(health.status, 200)
  }

  async _writeDenied(fragment: string) {
    await this.server._writeConfig(
      [
        'cache: true',
        'login_lockout_threshold: 9',
        'port: 9999',
        fragment,
        'routes:',
        '  - path: /reload-probe',
        '    respond:',
        '      status: 200',
        '      body: "v2-reloaded"',
        '',
      ]
        .filter(Boolean)
        .join('\n'),
    )
  }

  async aDeniedAddressIsRefusedServerWideAndBothWaysWithoutARestart() {
    await this._writeDenied('denied_ips: [203.0.113.7]')
    await new Promise((r) => setTimeout(r, 2_000))
    assert.equal(
      (await this.server._fetch('/aperio/health')).status,
      200,
      'denying an unrelated address leaves everybody else alone',
    )

    // The IPv6 entry is quoted because a bare `::1` in a flow sequence is a
    // yaml parse error, which would leave the previous config in place and
    // make this look like the deny list failing rather than the file failing.
    await this._writeDenied('denied_ips: [127.0.0.1, "::1"]')
    await waitFor(async () => (await this.server._fetch('/aperio/health')).status === 403, {
      label: 'the deny list to take effect',
    })

    // Server-wide, not just the proxy path.
    const cookie = await this.server._login()
    assert.equal((await this.server._fetch('/aperio/api/stats', { headers: { cookie } })).status, 403)
    assert.equal(
      (await this.server._fetch('/reload-probe', { host: 'probe.e2e.local' })).status,
      403,
      'the deny list is checked before a client-less route answers',
    )

    await this._writeDenied('')
    await waitFor(async () => (await this.server._fetch('/aperio/health')).status === 200, {
      label: 'access to be restored',
    })
  }

  async theFileWinsOverADashboardOverrideForTheSameKey() {
    const cookie = await this.server._login()
    const refused = await this.server._fetch('/aperio/api/settings', {
      method: 'PUT',
      headers: { cookie, 'content-type': 'application/json' },
      body: JSON.stringify({ login_lockout_threshold: 42 }),
    })
    assert.equal(refused.status, 400, 'an override for a key the file writes is refused')
    assert.equal(await this._setting('login_lockout_threshold'), 9, 'and it changed nothing')

    const allowed = await this.server._fetch('/aperio/api/settings', {
      method: 'PUT',
      headers: { cookie, 'content-type': 'application/json' },
      body: JSON.stringify({ cache_max_stale: 1800 }),
    })
    assert.equal(allowed.status, 200, 'a key the file leaves alone is still the dashboard’s')

    const settings = await this.server._api<Settings>('/aperio/api/settings')
    assert.ok(
      settings.file_keys.includes('login_lockout_threshold'),
      'the payload names the keys the file owns',
    )
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

  async aPerHostnameErrorPageIsServedAfterReload() {
    const page = join(this.server._dataDir, 'custom-504.html')
    await writeFile(page, '<h1>custom err.e2e.local 504</h1>\n')
    await this.server._writeConfig(
      ['cache: true', 'error_pages:', '  - hostname: err.e2e.local', `    504_page: ${page}`, ''].join(
        '\n',
      ),
    )
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

/** The report-and-exit modes, which never start a server. */
export class ServerCliSpec extends Test({ timeout: 90_000 }) {
  async _tmpConfig(yaml: string): Promise<string> {
    const dir = await mkdtemp(join(tmpdir(), 'aperio-cfg-'))
    const path = join(dir, 'aperio-server.yaml')
    await writeFile(path, yaml)
    return path
  }

  async printSchemaEmitsValidJson() {
    const res = await serverCli(['--print-schema'])
    assert.ok(res.ok)
    assert.match(res.out, /"ServerFileConfig"/)
    JSON.parse(res.out)
  }

  async printConfigAttributesEachValueAndMasksTheToken() {
    const path = await this._tmpConfig(
      ['max_body_size: 4242', 'trusted_proxies: [10.0.0.0/8]', 'headers:', '  request:', '    add:', '      X-A: b', ''].join('\n'),
    )
    const res = await serverCli(['--print-config'], {
      APERIO_SERVER_CONFIG: path,
      APERIO_SERVER_TOKEN: 'print-secret-token',
      APERIO_DATA_DIR: join(path, '..', 'print-data'),
    })
    assert.match(res.out, /APERIO_MAX_BODY_SIZE/)
    assert.match(res.out, /\[aperio-server\.yaml\]/, 'it says where the value came from')
    assert.match(res.out, /Structured aperio-server\.yaml sections: headers/)
    assert.doesNotMatch(res.out, /print-secret-token/, 'the master token is masked')
  }

  async checkConfigPassesAValidFileAndFailsAnInvalidOne() {
    const good = await this._tmpConfig(
      'server_token: e2e-lint-token-long-enough\nlb_strategy: sticky\n',
    )
    const ok = await serverCli(['--check-config'], { APERIO_SERVER_CONFIG: good })
    assert.ok(ok.ok, ok.out)
    assert.match(ok.out, /Configuration OK/)

    const bad = await this._tmpConfig(
      'server_token: e2e-lint-token-long-enough\nlb_strategy: bogus\nmax_body_size: not-a-number\n',
    )
    const fails = await serverCli(['--check-config'], { APERIO_SERVER_CONFIG: bad })
    assert.equal(fails.ok, false, 'an invalid config must exit non-zero')
    assert.match(fails.out, /FAIL/)
    assert.match(fails.out, /Configuration check FAILED/)
  }

  async theVersionDeclarationIsCheckedWithoutBeingNoisy() {
    const version = (await serverCli(['--version'])).out.trim().split(/\s+/).pop() ?? ''
    assert.ok(version, 'the server reports a version')

    const current = await this._tmpConfig(
      `version: ${version}\nserver_token: e2e-version-token-long-enough\n`,
    )
    const up = await serverCli(['--check-config'], { APERIO_SERVER_CONFIG: current })
    assert.ok(up.ok, up.out)
    assert.match(up.out, /matches this build/)

    // An older declaration is accepted: no recorded change applies, so a
    // clean upgrade stays quiet.
    const old = await this._tmpConfig('version: 0.1.0\nserver_token: e2e-version-token-long-enough\n')
    assert.ok((await serverCli(['--check-config'], { APERIO_SERVER_CONFIG: old })).ok)

    // A typo must never look like a clean upgrade.
    const typo = await this._tmpConfig(
      'version: not-a-version\nserver_token: e2e-version-token-long-enough\n',
    )
    const bad = await serverCli(['--check-config'], { APERIO_SERVER_CONFIG: typo })
    assert.equal(bad.ok, false)
    assert.match(bad.out, /not a version/)

    const none = await this._tmpConfig('server_token: e2e-version-token-long-enough\n')
    const quiet = await serverCli(['--check-config'], { APERIO_SERVER_CONFIG: none })
    assert.ok(quiet.ok)
    assert.match(quiet.out, /no `version:` declared/)
  }

  async aRemovedSettingRefusesTheStartInEitherSpelling() {
    for (const spelling of ['dashboard_auth: leftover', 'dashboard:\n  auth: leftover']) {
      const path = await this._tmpConfig(
        `server_token: e2e-removed-token-long-enough\n${spelling}\n`,
      )
      const res = await serverCli(['--check-config'], { APERIO_SERVER_CONFIG: path })
      assert.equal(res.ok, false, `${spelling} should refuse the start`)
      assert.match(res.out, /was removed/, spelling)
    }
    // The guard is specific: the same file without it is fine.
    const clean = await this._tmpConfig('server_token: e2e-removed-token-long-enough\n')
    assert.ok((await serverCli(['--check-config'], { APERIO_SERVER_CONFIG: clean })).ok)
  }
}

/** Binary frames bypass the tunnel's text compression, so the frame deflates
 *  its own payload; what has to hold is that the bytes still come back. */
export class CompressedTunnelServer extends AperioServerBase() {
  _configFile() {
    return 'tunnel_compression: true\n'
  }
}

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
