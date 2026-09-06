import { Test } from 'nole'
import assert from 'node:assert/strict'
import { sendRaw } from '../../lib/http.js'
import { ClientFor } from '../../lib/client.js'
import { BaseServerFor, BaseBackendFor, BaseClientFor } from './fixtures.js'

/** This file's own server: the specs below change it, so it is not
 *  shared with another file. See `fixtures.ts`. */
class OrgsServer extends BaseServerFor() {
  // The server's own panel hostname, for `PanelSpec`.
  _env() {
    return { ...super._env(), APERIO_DASHBOARD_HOSTNAME: 'panel.e2e.local' }
  }
}
class OrgsBackend extends BaseBackendFor() {}
class OrgsClient extends BaseClientFor(() => OrgsServer, () => OrgsBackend) {}

interface Org {
  id: string
  name: string
  custom_name?: string
  master?: boolean
  tokens?: number
}

/** A client using a fenced organization's wildcard token. */
export class FencedClient extends ClientFor(() => OrgsServer, () => OrgsBackend) {
  _token = ''
  _autoStart() {
    return false
  }
  _serverToken() {
    return this._token
  }
  _env() {
    return { APERIO_HOSTNAME: 'evil.e2e.local' }
  }
}

export class OrganizationsApiSpec extends Test({
  timeout: 90_000,
  dependencies: { server: () => OrgsServer },
}) {
  static acmeId = ''

  async _create(body: Record<string, unknown>) {
    return this.server._api<Org>('/aperio/api/orgs', {
      method: 'POST',
      body: JSON.stringify(body),
    })
  }

  async _status(path: string, method: string, body?: unknown): Promise<number> {
    const cookie = await this.server._login()
    const res = await this.server._fetch(path, {
      method,
      headers: { cookie, 'content-type': 'application/json' },
      body: body === undefined ? undefined : JSON.stringify(body),
    })
    return res.status
  }

  async anOrgIsCreatedAndListedBesideTheImplicitMaster() {
    const acme = await this._create({ name: 'acme', custom_name: 'Acme Inc.' })
    OrganizationsApiSpec.acmeId = acme.id

    const orgs = await this.server._api<Org[]>('/aperio/api/orgs')
    assert.ok(orgs.some((o) => o.id === 'master' && o.master === true))
    assert.ok(orgs.some((o) => o.name === 'acme'))
  }

  async aHandleIsAnIdentifierAndTheReservedNameIsRefused() {
    assert.equal(await this._status('/aperio/api/orgs', 'POST', { name: 'acme' }), 400)
    assert.equal(await this._status('/aperio/api/orgs', 'POST', { name: 'master' }), 400)
    // Anything that could be written a second way is refused at the source
    // rather than becoming an address nobody can reproduce.
    assert.equal(await this._status('/aperio/api/orgs', 'POST', { name: 'Acme Inc' }), 400)
  }

  async theDisplayNameMovesWithoutTheHandleMoving() {
    assert.equal(
      await this._status(
        `/aperio/api/orgs/${OrganizationsApiSpec.acmeId}/custom-name`,
        'PUT',
        { custom_name: 'Acme Global' },
      ),
      200,
    )
    const orgs = await this.server._api<Org[]>('/aperio/api/orgs')
    const acme = orgs.find((o) => o.id === OrganizationsApiSpec.acmeId)
    assert.equal(acme?.custom_name, 'Acme Global')
    assert.equal(acme?.name, 'acme', 'the handle it is addressed by did not move')
  }

  async theMasterOrgCannotBeDeleted() {
    assert.equal(await this._status('/aperio/api/orgs/master', 'DELETE'), 400)
  }
}

export class HostnameFenceSpec extends Test({
  // Ordered: this file's specs share one server and change it, so they
  // take turns. Files do not, they have a server each.
  after: () => [OrganizationsApiSpec],
  timeout: 120_000,
  dependencies: {
    server: () => OrgsServer,
    backend: () => OrgsBackend,
    main: () => OrgsClient,
    fenced: () => FencedClient,
  },
}) {
  static fencedId = ''

  async _status(path: string, method: string, body?: unknown): Promise<number> {
    const cookie = await this.server._login()
    const res = await this.server._fetch(path, {
      method,
      headers: { cookie, 'content-type': 'application/json' },
      body: body === undefined ? undefined : JSON.stringify(body),
    })
    return res.status
  }

  async _select(id: string) {
    assert.equal(await this._status('/aperio/api/orgs/select', 'POST', { id }), 200)
  }

  async aFenceIsDeclaredAndAnInvalidPatternIsRefused() {
    const fenced = await this.server._api<Org & { hostnames: string[] }>('/aperio/api/orgs', {
      method: 'POST',
      body: JSON.stringify({
        name: 'fenced',
        hostnames: ['fenced.e2e.local', '*.fenced.e2e.local'],
      }),
    })
    HostnameFenceSpec.fencedId = fenced.id
    assert.ok(fenced.hostnames.includes('fenced.e2e.local'), 'the response echoes the allowlist')

    assert.equal(
      await this._status('/aperio/api/orgs', 'POST', {
        name: 'broken',
        hostnames: ['app.*.com'],
      }),
      400,
    )
  }

  async aPartialLeftmostLabelIsALegalShapeAndTwoPlaceholdersAreNot() {
    const fleet = await this.server._api<Org & { hostnames: string[] }>('/aperio/api/orgs', {
      method: 'POST',
      body: JSON.stringify({ name: 'fleet', hostnames: ['*-pi.fleet.e2e.local'] }),
    })
    assert.ok(fleet.hostnames.includes('*-pi.fleet.e2e.local'))
    assert.equal(
      await this._status('/aperio/api/orgs', 'POST', {
        name: 'broken2',
        hostnames: ['*-pi-*.fleet.e2e.local'],
      }),
      400,
      'only the first placeholder could be free',
    )

    await this._select(fleet.id)
    assert.equal(
      await this._status('/aperio/api/tokens', 'POST', {
        name: 'in-fleet',
        hostnames: ['raspberry-pi.fleet.e2e.local'],
      }),
      200,
    )
    assert.equal(
      await this._status('/aperio/api/tokens', 'POST', {
        name: 'out-fleet',
        hostnames: ['raspberry-pie.fleet.e2e.local'],
      }),
      403,
    )
    // The domain around the fleet is not the fleet, or the pattern would just
    // mean *.fleet.e2e.local.
    assert.equal(
      await this._status('/aperio/api/tokens', 'POST', {
        name: 'plain-name',
        hostnames: ['test.fleet.e2e.local'],
      }),
      403,
    )
    assert.equal(
      await this._status('/aperio/api/tokens', 'POST', {
        name: 'in-fleet-2',
        hostnames: ['test-pi.fleet.e2e.local'],
      }),
      200,
    )
    await this._select('master')
  }

  async everySurfaceReadsTheSameFence() {
    await this._select(HostnameFenceSpec.fencedId)

    assert.equal(
      await this._status('/aperio/api/tokens', 'POST', {
        name: 'outside',
        hostnames: ['evil.e2e.local'],
      }),
      403,
    )
    assert.equal(
      await this._status('/aperio/api/tokens', 'POST', {
        name: 'inside',
        hostnames: ['app.fenced.e2e.local'],
      }),
      200,
    )

    // Ephemeral tunnels obey it.
    assert.equal(
      await this._status('/aperio/api/tunnels', 'POST', { name: 't', hostname: 'evil.e2e.local' }),
      403,
    )

    // Maintenance is wanted precisely when nothing of the org is connected.
    assert.equal(
      await this._status('/aperio/api/maintenance', 'POST', {
        hostname: 'fenced.e2e.local',
        enabled: true,
      }),
      200,
    )
    assert.equal(
      await this._status('/aperio/api/maintenance', 'POST', {
        hostname: 'evil.e2e.local',
        enabled: true,
      }),
      403,
    )
    assert.equal(
      await this._status('/aperio/api/maintenance', 'POST', {
        hostname: '*.fenced.e2e.local',
        enabled: true,
      }),
      200,
      'a subdomain wildcard needs a fence that owns the subtree',
    )
    assert.equal(
      await this._status('/aperio/api/maintenance', 'POST', {
        hostname: '*.e2e.local',
        enabled: true,
      }),
      403,
      'a subtree wider than the fence is refused',
    )

    const flags = await this.server._api<{ hostname: string }[]>('/aperio/api/maintenance')
    assert.ok(flags.some((f) => f.hostname === '*.fenced.e2e.local'), 'the wildcard is listed')

    // Share links read it too.
    assert.equal(
      await this._status('/aperio/api/share', 'POST', { hostname: 'app.fenced.e2e.local' }),
      200,
    )
    assert.equal(await this._status('/aperio/api/share', 'POST', { hostname: 'evil.e2e.local' }), 403)
  }

  async aMaintenanceFlagCarriesItsReasonAndItsWindow() {
    await this._status('/aperio/api/maintenance', 'POST', {
      hostname: 'app.fenced.e2e.local',
      enabled: true,
      reason: 'db migration',
      ttl_seconds: 900,
    })
    const flags = await this.server._api<{ hostname: string; reason?: string; until?: number }[]>(
      '/aperio/api/maintenance',
    )
    const flag = flags.find((f) => f.hostname === 'app.fenced.e2e.local')
    assert.equal(flag?.reason, 'db migration')
    assert.ok(flag?.until, 'the window it lifts at')

    assert.equal(
      await this._status('/aperio/api/maintenance', 'POST', {
        hostname: 'app.fenced.e2e.local',
        enabled: true,
        ttl_seconds: 1_800_000_000,
      }),
      400,
      'an absurd window is refused',
    )

    for (const hostname of ['app.fenced.e2e.local', '*.fenced.e2e.local', 'fenced.e2e.local']) {
      await this._status('/aperio/api/maintenance', 'POST', { hostname, enabled: false })
    }
  }

  async anOutOfFenceBindDeclaredByAClientIsDropped() {
    // A wildcard token stays legal: the fence narrows it when a client
    // connects, which is the thing under test.
    const wildcard = await this.server._api<{ token: string }>('/aperio/api/tokens', {
      method: 'POST',
      body: JSON.stringify({ name: 'fenced-wildcard', hostnames: ['*'] }),
    })
    this.fenced._token = wildcard.token
    await this.fenced._start()
    await new Promise((r) => setTimeout(r, 3_000))

    const res = await this.server._fetch('/hello', { host: 'evil.e2e.local' })
    assert.notEqual(res.status, 200, 'a fenced org bound a hostname outside its allowlist')

    // Clearing the fence lifts it.
    assert.equal(
      await this._status(`/aperio/api/orgs/${HostnameFenceSpec.fencedId}/hostnames`, 'PUT', {
        hostnames: [],
      }),
      200,
    )
    assert.equal(
      await this._status('/aperio/api/tokens', 'POST', {
        name: 'now-allowed',
        hostnames: ['evil.e2e.local'],
      }),
      200,
    )

    await this._select('master')
    await this.fenced._kill()
    await this.server._waitForClients(1)
  }
}

/** Two organizations have to behave like two separate installations. */
export class OrganizationIsolationSpec extends Test({
  // Ordered: this file's specs share one server and change it, so they
  // take turns. Files do not, they have a server each.
  after: () => [HostnameFenceSpec],
  timeout: 120_000,
  dependencies: { server: () => OrgsServer, client: () => OrgsClient },
}) {
  async _select(id: string) {
    const cookie = await this.server._login()
    const res = await this.server._fetch('/aperio/api/orgs/select', {
      method: 'POST',
      headers: { cookie, 'content-type': 'application/json' },
      body: JSON.stringify({ id }),
    })
    assert.equal(res.status, 200)
  }

  async whatIsCreatedInAChildOrgIsVisibleOnlyThere() {
    await this._select(OrganizationsApiSpec.acmeId)

    await this.server._api('/aperio/api/tokens', {
      method: 'POST',
      body: JSON.stringify({ name: 'acme-token', hostnames: ['*'] }),
    })
    const tokens = await this.server._api<{ name: string }[]>('/aperio/api/tokens')
    assert.ok(tokens.some((t) => t.name === 'acme-token'))

    const audit = await this.server._api<{ detail?: string }[]>('/aperio/api/audit')
    assert.ok(
      JSON.stringify(audit).includes('name=acme-token'),
      "the child org's audit shows its own token creation",
    )

    // The master admin's own session belongs to master, so it is hidden here.
    const sessions = await this.server._api<{ current?: boolean }[]>('/aperio/api/sessions')
    assert.ok(!sessions.some((s) => s.current), 'the master session appears in a child org')

    await this.server._api('/aperio/api/webhooks', {
      method: 'POST',
      body: JSON.stringify({
        name: 'acme-hook',
        url: 'http://127.0.0.1:1/',
        events: ['token_created'],
      }),
    })
    const hooks = await this.server._api<{ name: string }[]>('/aperio/api/webhooks')
    assert.ok(hooks.some((h) => h.name === 'acme-hook'))
  }

  async aNamedAdminOfAChildOrgIsSandboxedToIt() {
    await this.server._api('/aperio/api/users', {
      method: 'POST',
      body: JSON.stringify({ username: 'acme-admin', password: 'acmepass123', role: 'admin' }),
    })
    const cookies = await sendRaw(this.server._url, '/aperio/auth', {
      method: 'POST',
      headers: {
        authorization: `Basic ${Buffer.from('acme-admin:acmepass123').toString('base64')}`,
      },
    })
    const cookie = cookies.at(0)?.split(';')[0]
    assert.ok(cookie, 'the child-org admin can sign in')

    // Server-global surfaces are master's alone.
    for (const path of ['/aperio/api/settings', '/aperio/api/orgs', '/aperio/api/export']) {
      const res = await this.server._fetch(path, { headers: { cookie } })
      assert.equal(res.status, 403, path)
    }

    const tokens = await this.server._json<{ name: string }[]>('/aperio/api/tokens', {
      headers: { cookie },
    })
    assert.ok(tokens.some((t) => t.name === 'acme-token'))
    const users = await this.server._json<{ username: string }[]>('/aperio/api/users', {
      headers: { cookie },
    })
    assert.ok(users.some((u) => u.username === 'acme-admin'))
  }

  async masterSeesNoneOfIt() {
    await this._select('master')

    const tokens = await this.server._api<{ name: string }[]>('/aperio/api/tokens')
    assert.ok(!tokens.some((t) => t.name === 'acme-token'), 'a child token leaked into master')

    const audit = await this.server._api<unknown[]>('/aperio/api/audit')
    assert.ok(
      !JSON.stringify(audit).includes('name=acme-token'),
      "a child org's audit event leaked into master",
    )

    const sessions = await this.server._api<{ current?: boolean }[]>('/aperio/api/sessions')
    assert.ok(sessions.some((s) => s.current), 'the master session is visible again in master')

    const hooks = await this.server._api<{ name: string }[]>('/aperio/api/webhooks')
    assert.ok(!hooks.some((h) => h.name === 'acme-hook'), 'a child webhook leaked into master')

    const orgs = await this.server._api<Org[]>('/aperio/api/orgs')
    const acme = orgs.find((o) => o.id === OrganizationsApiSpec.acmeId)
    assert.equal(acme?.tokens, 1, "the listing still counts the child org's token")
  }
}

/** One user, several organizations: created from master with a grant per
 *  organization, it switches between them and reaches nothing else, and an
 *  Admin of master is the server without being every tenant. */
export class GrantsSpec extends Test({
  after: () => [OrganizationIsolationSpec],
  timeout: 120_000,
  dependencies: { server: () => OrgsServer },
}) {
  static betaId = ''

  async _signIn(user: string, password: string): Promise<string> {
    const cookies = await sendRaw(this.server._url, '/aperio/auth', {
      method: 'POST',
      headers: {
        authorization: `Basic ${Buffer.from(`${user}:${password}`).toString('base64')}`,
      },
    })
    const raw = cookies.at(0)
    assert.ok(raw, `${user} could not sign in`)
    return raw.split(';')[0]
  }

  async _selectAs(cookie: string, id: string): Promise<number> {
    const res = await this.server._fetch('/aperio/api/orgs/select', {
      method: 'POST',
      headers: { cookie, 'content-type': 'application/json' },
      body: JSON.stringify({ id }),
    })
    return res.status
  }

  async aUserGrantedTwoOrganizationsSwitchesBetweenThemAndNowhereElse() {
    const acmeId = OrganizationsApiSpec.acmeId
    const beta = await this.server._api<Org>('/aperio/api/orgs', {
      method: 'POST',
      body: JSON.stringify({ name: 'beta' }),
    })
    GrantsSpec.betaId = beta.id
    await this.server._api('/aperio/api/users', {
      method: 'POST',
      body: JSON.stringify({
        username: 'two-orgs',
        password: 'twoorgs123',
        grants: [
          { org: acmeId, role: 'admin' },
          { org: beta.id, role: 'viewer' },
        ],
      }),
    })
    const cookie = await this._signIn('two-orgs', 'twoorgs123')

    const session = await this.server._json<{
      master_admin: boolean
      selected_org: string
      orgs: { id: string; role: string }[]
    }>('/aperio/api/session', { headers: { cookie } })
    assert.equal(session.master_admin, false)
    assert.deepEqual(
      session.orgs.map((o) => o.id).sort(),
      [acmeId, beta.id].sort(),
      'the session lists exactly the organizations a grant reaches',
    )
    assert.ok([acmeId, beta.id].includes(session.selected_org), 'lands in one of them')

    // Viewer in Beta: a mutation is refused by the role floor there.
    assert.equal(await this._selectAs(cookie, beta.id), 200)
    const asViewer = await this.server._fetch('/aperio/api/tokens', {
      method: 'POST',
      headers: { cookie, 'content-type': 'application/json' },
      body: JSON.stringify({ name: 'nope' }),
    })
    assert.equal(asViewer.status, 403)

    // Admin in Acme: the token made there earlier is visible.
    assert.equal(await this._selectAs(cookie, acmeId), 200)
    const tokens = await this.server._json<{ name: string }[]>('/aperio/api/tokens', {
      headers: { cookie },
    })
    assert.ok(tokens.some((t) => t.name === 'acme-token'))

    // Master and the organization listing are out of reach.
    assert.equal(await this._selectAs(cookie, 'master'), 403)
    assert.equal((await this.server._fetch('/aperio/api/orgs', { headers: { cookie } })).status, 403)
  }

  async anAdminOfMasterIsNotEveryOrganizationAndCannotMintStar() {
    await this.server._api('/aperio/api/users', {
      method: 'POST',
      body: JSON.stringify({ username: 'master-admin', password: 'masteradmin1', role: 'admin' }),
    })
    const cookie = await this._signIn('master-admin', 'masteradmin1')
    // The server-global surface answers.
    assert.equal((await this.server._fetch('/aperio/api/orgs', { headers: { cookie } })).status, 200)
    // A child is not theirs to act in.
    assert.equal(await this._selectAs(cookie, GrantsSpec.betaId), 403)
    // And `*` is given only by a holder of `*`.
    const res = await this.server._fetch('/aperio/api/users', {
      method: 'POST',
      headers: { cookie, 'content-type': 'application/json' },
      body: JSON.stringify({
        username: 'star',
        password: 'starstar123',
        grants: [{ org: '*', role: 'viewer' }],
      }),
    })
    assert.equal(res.status, 403)
  }
}

/** A hostname whose root is the dashboard: the server's own from the
 *  environment, an organization's chosen inside its fence. `/aperio` stays
 *  everywhere; a bind may not claim the name; the organization's login admits
 *  its own people and the super-admin. */
export class PanelSpec extends Test({
  after: () => [GrantsSpec],
  timeout: 120_000,
  dependencies: { server: () => OrgsServer },
}) {
  async _signInOn(host: string, user: string, password: string): Promise<number> {
    const res = await this.server._fetch('/aperio/auth', {
      method: 'POST',
      host,
      headers: {
        authorization: `Basic ${Buffer.from(`${user}:${password}`).toString('base64')}`,
      },
    })
    return res.status
  }

  async theServersOwnPanelServesTheDashboardAtItsRoot() {
    // Anonymous: the dashboard sends the browser to the login, and back to
    // `/` afterwards rather than to `/aperio`.
    const root = await this.server._fetch('/', { host: 'panel.e2e.local' })
    assert.equal(root.status, 302)
    assert.equal(root.headers['location'], '/aperio/auth?redirect=/')
    // With a session: the same page `/aperio` serves.
    const cookie = await this.server._login()
    const panel = await this.server._fetch('/', { host: 'panel.e2e.local', headers: { cookie } })
    assert.equal(panel.status, 200)
    assert.ok(panel.body.includes('<html'), 'the dashboard page')
    const api = await this.server._fetch('/api/session', {
      host: 'panel.e2e.local',
      headers: { cookie },
    })
    assert.equal(api.status, 200)
    assert.equal(JSON.parse(api.body).username, 'aperio')
    // And `/aperio/...` keeps resolving there too.
    const long = await this.server._fetch('/aperio/api/session', {
      host: 'panel.e2e.local',
      headers: { cookie },
    })
    assert.equal(long.status, 200)
    // The login page knows whose panel it is on.
    const health = await this.server._json<{ panel?: { org: string } }>('/aperio/health', {
      host: 'panel.e2e.local',
    })
    assert.equal(health.panel?.org, 'master')
  }

  async anOrganizationPicksAPanelInsideItsFenceAndItsLoginIsItsOwn() {
    const acmeId = OrganizationsApiSpec.acmeId
    await this.server._api(`/aperio/api/orgs/${acmeId}/hostnames`, {
      method: 'PUT',
      body: JSON.stringify({ hostnames: ['*.acme.e2e.local'] }),
    })
    // Outside the fence: refused. Inside: taken.
    const outside = await this.server._fetch(`/aperio/api/orgs/${acmeId}/panel`, {
      method: 'PUT',
      headers: { cookie: await this.server._login(), 'content-type': 'application/json' },
      body: JSON.stringify({ hostname: 'panel.beta.e2e.local' }),
    })
    assert.equal(outside.status, 403)
    const set = await this.server._api<{ panel_hostname: string }>(`/aperio/api/orgs/${acmeId}/panel`, {
      method: 'PUT',
      body: JSON.stringify({ hostname: 'aperio.acme.e2e.local' }),
    })
    assert.equal(set.panel_hostname, 'aperio.acme.e2e.local')

    // The root is the login, and the login page names the organization.
    const root = await this.server._fetch('/', { host: 'aperio.acme.e2e.local' })
    assert.equal(root.status, 302)
    const health = await this.server._json<{ panel?: { org: string; name: string } }>('/aperio/health', {
      host: 'aperio.acme.e2e.local',
    })
    assert.equal(health.panel?.name, 'acme')

    // Acme's admin (from the isolation spec) and the two-org user (Admin in
    // Acme) get in; an Admin of master alone does not, with the answer a
    // wrong password gets; the built-in account always does.
    assert.equal(await this._signInOn('aperio.acme.e2e.local', 'acme-admin', 'acmepass123'), 200)
    assert.equal(await this._signInOn('aperio.acme.e2e.local', 'two-orgs', 'twoorgs123'), 200)
    assert.equal(await this._signInOn('aperio.acme.e2e.local', 'master-admin', 'masteradmin1'), 401)
    assert.equal(await this._signInOn('aperio.acme.e2e.local', 'master-admin', 'wrong-password'), 401)
    assert.equal(
      await this._signInOn('aperio.acme.e2e.local', 'aperio', this.server._token),
      200,
    )
    // Elsewhere the master admin is still let in.
    assert.equal(await this._signInOn('tunnel.e2e.local', 'master-admin', 'masteradmin1'), 200)

    // A panel serves the panel and nothing else: no token may bind it.
    const bind = await this.server._fetch('/aperio/api/tokens', {
      method: 'POST',
      headers: { cookie: await this.server._login(), 'content-type': 'application/json' },
      body: JSON.stringify({ name: 'on-panel', hostnames: ['aperio.acme.e2e.local'] }),
    })
    assert.equal(bind.status, 403)
  }
}
