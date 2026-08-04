import { Test } from 'nole'
import assert from 'node:assert/strict'
import { sendRaw } from '../../lib/http.js'
import { ClientFor } from '../../lib/client.js'
import { BaseServer, BaseBackend, BaseClient, HOST } from './fixtures.js'
import { EdgeIntegrationSpec } from './webhooks.test.js'

interface Org {
  id: string
  name: string
  custom_name?: string
  master?: boolean
  tokens?: number
}

/** A client using a fenced organization's wildcard token. */
export class FencedClient extends ClientFor(() => BaseServer, () => BaseBackend) {
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
  // Ordered rather than left to overlap: these specs share one
  // server and change it, so under `--concurrency` they would contend.
  after: () => [EdgeIntegrationSpec],
  timeout: 90_000,
  dependencies: { server: () => BaseServer },
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
  // Ordered rather than left to overlap: these specs share one
  // server and change it, so under `--concurrency` they would contend.
  after: () => [OrganizationsApiSpec],
  timeout: 120_000,
  dependencies: {
    server: () => BaseServer,
    backend: () => BaseBackend,
    main: () => BaseClient,
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
  // Ordered rather than left to overlap: these specs share one
  // server and change it, so under `--concurrency` they would contend.
  after: () => [HostnameFenceSpec],
  timeout: 120_000,
  dependencies: { server: () => BaseServer, client: () => BaseClient },
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
