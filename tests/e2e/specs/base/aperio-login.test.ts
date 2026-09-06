import { Test } from 'nole'
import assert from 'node:assert/strict'
import { BaseServerFor, BaseBackendFor, BaseClientFor, HOST } from './fixtures.js'

/** This file's own server: its gate is `auth: {method: aperio}`, which would
 *  put every other file's plain proxied request behind a login. */
class AperioLoginServer extends BaseServerFor() {
  _configFile() {
    return ['server:', '  auth:', '    method: aperio', ''].join('\n')
  }
}
class AperioLoginBackend extends BaseBackendFor() {}
class AperioLoginClient extends BaseClientFor(() => AperioLoginServer, () => AperioLoginBackend) {}

/** `auth: {method: aperio}`: a route that says "sign in" without inventing a
 *  password. A browser is sent to the login, a script gets 401, and a
 *  dashboard session is admitted. */
export class AperioLoginSpec extends Test({
  timeout: 90_000,
  dependencies: { server: () => AperioLoginServer, client: () => AperioLoginClient },
}) {
  async aBrowserIsSentToTheLoginAndAScriptGets401() {
    const browser = await this.server._fetch('/hello', {
      headers: { accept: 'text/html,application/xhtml+xml' },
    })
    assert.equal(browser.status, 302)
    assert.ok(
      (browser.headers['location'] ?? '').startsWith('/aperio/auth?redirect='),
      `the login page, got ${browser.headers['location']}`,
    )
    const script = await this.server._fetch('/hello')
    assert.equal(script.status, 401)
    assert.equal(script.headers['www-authenticate'], undefined, 'nothing a script could answer with')
  }

  async aDashboardSessionIsAdmitted() {
    const cookie = await this.server._login()
    const res = await this.server._fetch('/hello', { headers: { cookie } })
    assert.equal(res.status, 200)
    assert.ok(res.body.length > 0, 'the backend answered')
  }

  async theExplainReportNamesTheGate() {
    const report = await this.server._api<{ steps: { code: string }[] }>(
      `/aperio/api/explain?hostname=${HOST}&path=/hello`,
    )
    assert.ok(
      report.steps.some((s) => s.code === 'visitor_gate.aperio_login'),
      `explain names the aperio gate: ${JSON.stringify(report.steps.map((s) => s.code))}`,
    )
  }
}
