import { Test } from 'nole'
import assert from 'node:assert/strict'
import { sendRaw } from '../../lib/http.js'
import { BaseServerFor } from './fixtures.js'

/** This file's own server, with the login fence on: its specs change it,
 *  and the fence would refuse the other files' cross-hostname logins. */
class FencedServer extends BaseServerFor() {
  _env() {
    return { ...super._env(), APERIO_DASHBOARD_FENCED_LOGIN: '1' }
  }
}

interface Org {
  id: string
  name: string
}

/** Under `dashboard.fenced_login`, a tenant's hostname admits the tenant's
 *  own people and anyone reaching master; a session is good on the hostname
 *  it was minted on and nowhere else. */
export class FencedLoginSpec extends Test({
  timeout: 120_000,
  dependencies: { server: () => FencedServer },
}) {
  async _signInOn(host: string, user: string, password: string): Promise<{ status: number; cookie?: string }> {
    const cookies = await sendRaw(this.server._url, '/aperio/auth', {
      method: 'POST',
      host,
      headers: {
        authorization: `Basic ${Buffer.from(`${user}:${password}`).toString('base64')}`,
      },
    }).catch(() => null)
    if (!cookies) return { status: 401 }
    const raw = cookies.at(0)
    return raw ? { status: 200, cookie: raw.split(';')[0] } : { status: 401 }
  }

  async aTenantsHostnameAdmitsTheTenantMasterAndTheUnfencedAndNobodyElse() {
    const acme = await this.server._api<Org>('/aperio/api/orgs', {
      method: 'POST',
      body: JSON.stringify({ name: 'acme', hostnames: ['*.acme.e2e.local'] }),
    })
    const beta = await this.server._api<Org>('/aperio/api/orgs', {
      method: 'POST',
      body: JSON.stringify({ name: 'beta', hostnames: ['*.beta.e2e.local'] }),
    })
    const open = await this.server._api<Org>('/aperio/api/orgs', {
      method: 'POST',
      body: JSON.stringify({ name: 'open' }),
    })
    for (const [username, grants] of [
      ['acme-user', [{ org: acme.id, role: 'viewer' }]],
      ['beta-user', [{ org: beta.id, role: 'admin' }]],
      ['open-user', [{ org: open.id, role: 'viewer' }]],
      ['master-user', [{ org: 'master', role: 'viewer' }]],
    ] as const) {
      await this.server._api('/aperio/api/users', {
        method: 'POST',
        body: JSON.stringify({ username, password: 'fence-password', grants }),
      })
    }

    const acmeHost = 'www.acme.e2e.local'
    assert.equal((await this._signInOn(acmeHost, 'acme-user', 'fence-password')).status, 200)
    assert.equal((await this._signInOn(acmeHost, 'master-user', 'fence-password')).status, 200)
    assert.equal((await this._signInOn(acmeHost, 'open-user', 'fence-password')).status, 200)
    assert.equal((await this._signInOn(acmeHost, 'beta-user', 'fence-password')).status, 401)
    assert.equal((await this._signInOn(acmeHost, 'beta-user', 'wrong-password')).status, 401)
    // A hostname no fence claims is master's.
    assert.equal((await this._signInOn('tunnel.e2e.local', 'master-user', 'fence-password')).status, 200)
    assert.equal((await this._signInOn('tunnel.e2e.local', 'open-user', 'fence-password')).status, 200)
    assert.equal((await this._signInOn('tunnel.e2e.local', 'acme-user', 'fence-password')).status, 401)

    // The login page names the organization whose hostname this is.
    const health = await this.server._json<{ login_org?: { name: string } }>('/aperio/health', {
      host: acmeHost,
    })
    assert.equal(health.login_org?.name, 'acme')
  }

  async aSessionIsGoodOnTheHostnameItWasMintedOnAndNowhereElse() {
    const { status, cookie } = await this._signInOn('www.acme.e2e.local', 'acme-user', 'fence-password')
    assert.equal(status, 200)
    assert.ok(cookie)
    const here = await this.server._fetch('/aperio/api/session', {
      host: 'www.acme.e2e.local',
      headers: { cookie },
    })
    assert.equal(here.status, 200)
    assert.equal(JSON.parse(here.body).username, 'acme-user')
    const elsewhere = await this.server._fetch('/aperio/api/session', {
      host: 'www.beta.e2e.local',
      headers: { cookie },
    })
    assert.notEqual(elsewhere.status, 200, 'a cookie lifted to another hostname opens nothing')
  }
}
