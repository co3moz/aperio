import { Test } from 'nole'
import assert from 'node:assert/strict'
import { AperioServerBase } from '../../lib/server.js'
import { sendRaw } from '../../lib/http.js'

export class SessionServer extends AperioServerBase() {
  // Overridden rather than passed as `{ env }`, because the origin has to
  // name the port *this instance* was given, which does not exist until it
  // starts. In bash the same line is `http://localhost:18100`, a constant,
  // and it is only correct because every phase there is pinned to that port.
  _env() {
    return { APERIO_WEBAUTHN_ORIGIN: `http://localhost:${this._port}` }
  }
}

interface SessionView {
  id: string
  username: string
  role?: string
  current?: boolean
  ip?: string
}

/** A dashboard session is server-side state, so a restart is the test. */
export class SessionRestartSpec extends Test({
  timeout: 90_000,
  dependencies: { server: () => SessionServer },
}) {
  static userCookie = ''

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

  async aUsersSessionSurvivesAServerRestart() {
    await this.server._api('/aperio/api/users', {
      method: 'POST',
      body: JSON.stringify({
        username: 'e2e-restart',
        password: 'restart-password',
        role: 'operator',
      }),
    })
    SessionRestartSpec.userCookie = await this._signIn('e2e-restart', 'restart-password')

    await this.server._restart()

    const session = await this.server._json<{ username: string; role: string }>(
      '/aperio/api/session',
      { headers: { cookie: SessionRestartSpec.userCookie } },
    )
    assert.equal(session.username, 'e2e-restart')
    assert.equal(session.role, 'operator', 'the restored session kept its role')
  }

  async theAdminsOwnSessionSurvivesToo() {
    const session = await this.server._json<{ username: string }>('/aperio/api/session', {
      headers: { cookie: await this.server._login() },
    })
    assert.equal(session.username, 'aperio')
  }
}

export class SessionManagementSpec extends Test({
  timeout: 90_000,
  after: () => [SessionRestartSpec],
  dependencies: { server: () => SessionServer },
}) {
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

  async theListingShowsEverySessionWithItsMetadata() {
    const sessions = await this.server._api<SessionView[]>('/aperio/api/sessions')
    assert.ok(sessions.some((s) => s.username === 'e2e-restart'))
    assert.ok(sessions.some((s) => s.username === 'aperio'))
    assert.ok(sessions.some((s) => s.current === true), "the caller's own session is marked")
    assert.ok(sessions.some((s) => s.ip === '127.0.0.1'), 'the sign-in IP is recorded')
  }

  async theListingIsAdminOnly() {
    const res = await this.server._fetch('/aperio/api/sessions', {
      headers: { cookie: SessionRestartSpec.userCookie },
    })
    assert.equal(res.status, 403)
  }

  async revokingASessionStopsItsCookieAtOnce() {
    const sessions = await this.server._api<SessionView[]>('/aperio/api/sessions')
    const mine = sessions.find((s) => s.username === 'e2e-restart')
    assert.ok(mine, 'the user session is listed')

    const cookie = await this.server._login()
    const revoked = await this.server._fetch(`/aperio/api/sessions/${mine.id}`, {
      method: 'DELETE',
      headers: { cookie },
    })
    assert.equal(revoked.status, 200)

    const after = await this.server._fetch('/aperio/api/stats', {
      headers: { cookie: SessionRestartSpec.userCookie },
    })
    assert.equal(after.status, 302, 'a revoked session is refused immediately')
  }

  async signOutEverywhereSpareTheCaller() {
    SessionRestartSpec.userCookie = await this._signIn('e2e-restart', 'restart-password')
    const cookie = await this.server._login()

    const cleared = await this.server._json<{ ended: number }>('/aperio/api/sessions', {
      method: 'DELETE',
      headers: { cookie },
    })
    assert.equal(typeof cleared.ended, 'number', 'it reports how many it ended')

    const other = await this.server._fetch('/aperio/api/session', {
      headers: { cookie: SessionRestartSpec.userCookie },
    })
    assert.equal(other.status, 302, 'the other session is gone')

    const own = await this.server._fetch('/aperio/api/stats', { headers: { cookie } })
    assert.equal(own.status, 200, "the caller's own session survives")
  }

  async loggingOutRemovesTheSessionDurably() {
    const cookie = await this._signIn('e2e-restart', 'restart-password')
    const out = await this.server._fetch('/aperio/auth/logout', { method: 'POST', headers: { cookie } })
    assert.equal(out.status, 200)

    const after = await this.server._fetch('/aperio/api/stats', { headers: { cookie } })
    assert.equal(after.status, 302)
  }
}

/** The usernameless ceremony, and that garbage never gets past it. */
export class DiscoverablePasskeySpec extends Test({
  timeout: 60_000,
  dependencies: { server: () => SessionServer },
}) {
  async aDiscoverableCeremonyStarts() {
    const started = await this.server._json<{ ceremony_id: string; challenge: string }>(
      '/aperio/auth/passkey/discoverable/start',
      { method: 'POST' },
    )
    assert.ok(started.ceremony_id)
    assert.ok(started.challenge)

    const finished = await this.server._fetch('/aperio/auth/passkey/discoverable/finish', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ ceremony_id: started.ceremony_id, credential: {} }),
    })
    assert.ok([400, 401, 422].includes(finished.status), `got ${finished.status}`)
  }

  async anUnknownCeremonyIsRejected() {
    const res = await this.server._fetch('/aperio/auth/passkey/discoverable/finish', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ ceremony_id: 'nope', credential: {} }),
    })
    assert.ok([400, 422].includes(res.status), `got ${res.status}`)
  }
}
