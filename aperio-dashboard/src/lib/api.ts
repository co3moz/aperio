// Typed client for the aperio-server dashboard API. Shapes mirror the
// Serialize structs in aperio-server (main.rs, stats.rs, tokens.rs, ...).

export interface PeriodStats {
  requests: number
  success: number
  failed: number
  bytes_sent: number
  bytes_received: number
  duration_ms: number
}

export interface PersistentStats {
  total_requests: number
  total_success: number
  total_failed: number
  total_bytes_sent: number
  total_bytes_received: number
  total_request_duration_ms: number
  periods: Record<string, PeriodStats>
  by_token: Record<string, PeriodStats>
  by_hostname: Record<string, PeriodStats>
}

export interface ClientDetail {
  id: string
  ip: string
  connected_for_seconds: number
  request_count: number
  path_bind: string | null
  hostname_binds: string[]
  token_name: string | null
  override_path_bind: string | null
  override_hostname_bind: string | null
  last_ping_seconds_ago: number | null
  max_concurrent: number | null
  version: string | null
  service: string | null
  public: boolean
  visitor_auth: boolean
  allowed_ips: string[]
  protocol: number | null
  protocol_mismatch: boolean
  backend_healthy: boolean
  /** False only while a configured health check hasn't completed its first probe. */
  backend_probed: boolean
  priority: number
  bandwidth_bps: number | null
  healthy: boolean
  draining: boolean
  enabled: boolean
  instance_id: string | null
  instance_id_shared: boolean
  /** Process-wide instance group (the client's raw client_id base), shared by
   * every service and parallel connection of one client process. Used to group
   * connections in the UI so a multi-connection client shows as one entity.
   * null for clients that predate the x-aperio-instance header. */
  instance_group: string | null
}

export interface ServerStats {
  total_requests: number
  successful_requests: number
  failed_requests: number
  total_bytes_transferred: number
  connected_clients_count: number
  uptime_seconds: number
  pending_requests_count: number
  active_clients: ClientDetail[]
  persistent: PersistentStats
  avg_response_ms: number
  today: PeriodStats
}

export interface RequestLog {
  id: string
  timestamp: string
  method: string
  uri: string
  status: number | null
  duration_ms: number
  error: string | null
  /** Request hostname (absent for failures resolved before routing). */
  host?: string | null
}

export interface CapturedRequest {
  id: string
  timestamp: string
  method: string
  uri: string
  req_headers: [string, string][]
  req_body: string | null
  req_body_truncated: boolean
  status: number
  resp_headers: [string, string][]
  resp_body: string | null
  resp_body_truncated: boolean
  resp_streamed: boolean
  duration_ms: number
  timeline?: RequestTimeline
}

/** Microsecond offsets from the server first receiving the request; client
 *  stages are estimated anchors (transit split evenly) when present. */
export interface RequestTimeline {
  dispatched_us: number
  client_received_us?: number
  backend_sent_us?: number
  backend_first_byte_us?: number
  backend_done_us?: number
  client_responded_us?: number
  response_received_us: number
  finished_us: number
  estimated_anchor: boolean
}

export interface StageStat {
  stage: string
  count: number
  mean_us: number
  stddev_us: number
  last_us?: number | null
  anomalous: boolean
}

export interface RouteStageStats {
  host: string
  stages: StageStat[]
}

export interface ReplayResult {
  status: number
  duration_ms: number
}

export interface TokenView {
  id: string
  name: string
  token_prefix: string
  hostnames: string[]
  paths: string[]
  allowed_ips: string[]
  created_at: number
  expires_at: number | null
  expired: boolean
  max_rps: number | null
  daily_max_bytes: number | null
  allow_public: boolean
  canary: boolean
}

export interface AdminKeyView {
  id: string
  name: string
  key_prefix: string
  role: 'viewer' | 'operator' | 'admin'
  org_id: string | null
  created_at: number
  expires_at: number | null
  expired: boolean
}

export interface AdminKeyCreatePayload {
  name: string
  role: string
  org_id?: string
  ttl_seconds?: number
}

export interface TokenCreatePayload {
  name: string
  hostnames: string[]
  paths: string[]
  allowed_ips: string[]
  ttl_seconds?: number
  max_rps?: number
  daily_max_bytes?: number
  allow_public?: boolean
  canary?: boolean
}

export interface TokenUpdatePayload {
  hostnames: string[]
  paths: string[]
  allowed_ips: string[]
  ttl_seconds?: number
  max_rps?: number
  daily_max_bytes?: number
  allow_public?: boolean
  canary?: boolean
}

export interface Webhook {
  id: string
  name: string
  url: string
  events: string[]
  enabled: boolean
  created_at: number
  /** Delivery payload format: generic JSON or a ready-made chat message. */
  format: 'generic' | 'slack' | 'discord' | 'teams'
  /** True when deliveries are HMAC-signed (the secret itself is never returned). */
  signed: boolean
}

export interface WebhookDelivery {
  id: string
  webhook_id: string
  webhook_name: string
  event: string
  /** RFC3339 time of the first attempt. */
  timestamp: string
  success: boolean
  /** HTTP status of the last attempt (absent = the request never completed). */
  status?: number
  error?: string
  attempts: number
  duration_ms: number
  /** The exact payload sent (truncated for storage). */
  body: string
  created_at: number
}

export interface HistoryBucket {
  /** Period label: 2026-07-06, 2026-W27, 2026-07, or 2026. */
  period: string
  requests: number
  success: number
  failed: number
  bytes_sent: number
  bytes_received: number
  avg_ms: number
}

export interface UptimeDay {
  date: string
  up_secs: number
  degraded_secs: number
  down_secs: number
}

export interface UptimeEntry {
  name: string
  status: 'up' | 'degraded' | 'down'
  last_seen: number
  pct_today: number | null
  pct_7d: number | null
  pct_30d: number | null
  days: UptimeDay[]
}

export interface PasskeyInfo {
  id: string
  name: string
  created_at: number
  /** Signs in from the login page without a username. */
  usernameless?: boolean
}

export interface AuditEvent {
  ts: number
  timestamp: string
  event: string
  /** Who performed the action: username, 'aperio', 'system', or '-'. */
  actor: string
  actor_ip: string
  /** Organization the event belongs to (null = the implicit master org). The
   *  list is already server-filtered to the caller's effective org. */
  org_id?: string | null
  details: string
}

/** Dashboard-editable server settings (see SettingsOverrides in the server). */
export type SettingsValues = Record<string, string | number | boolean | null>

export type SettingsOverrides = Record<string, string | number | boolean | null | undefined>

export interface EnvFlag {
  key: string
  value: string
}

export interface EnvironmentReport {
  /** "docker" when the server runs in a container, else "native". */
  runtime: 'docker' | 'native'
  flags: EnvFlag[]
}

export interface SettingsPayload {
  effective: SettingsValues
  defaults: SettingsValues
  overrides: SettingsOverrides
  /** Read-only env-only flags for the reference table. */
  environment: EnvironmentReport
}

export type Role = 'viewer' | 'operator' | 'admin'

export interface SessionInfo {
  expires_in_seconds: number
  username: string
  role: Role
  /** True when the session's user has TOTP two-factor auth enabled. */
  totp: boolean
  /** True for the built-in `aperio` super-admin, who may switch organizations. */
  master_admin: boolean
  /** The organization the session currently views (`master` or a child id). */
  selected_org: string
}

/** An organization as listed for the master super-admin. */
export interface Organization {
  /** `master` for the implicit master org, otherwise a child org UUID. */
  id: string
  name: string
  /** True for the implicit master organization. */
  master: boolean
  /** Unix seconds of creation (absent for master). */
  created_at?: number
  /** Number of dashboard users in this org. */
  users: number
  /** Number of API tokens in this org. */
  tokens: number
}

export interface OrgQuota {
  max_clients?: number
  max_tokens?: number
  max_users?: number
  max_bytes_month?: number
}

export interface SelfHealth {
  uptime_seconds: number
  connected_clients: number
  rss_bytes: number | null
  store_bytes: number
  cache: { entries: number; bytes: number; hits: number; misses: number; hit_ratio: number }
}

export interface CacheStats {
  entries: number
  bytes: number
  hits: number
  misses: number
  hit_ratio: number
}

export interface OrgOidcPayload {
  issuer: string
  client_id: string
  client_secret: string
  allowed_emails: string[]
}

export interface OrgUsage {
  org_id: string
  month: string
  requests: number
  bytes: number
  clients: number
  tokens: number
  users: number
  quota: {
    max_clients: number | null
    max_tokens: number | null
    max_users: number | null
    max_bytes_month: number | null
  } | null
}

export interface DashboardUser {
  id: string
  username: string
  role: Role
  created_at: number
  enabled: boolean
  /** True when this user has TOTP two-factor auth enabled. */
  totp: boolean
}

export interface LiveSession {
  /** Hashed-token id — usable for revocation, useless for hijacking. */
  id: string
  username: string
  role: string
  /** Set for visitor-password sessions scoped to one host. */
  scope_host?: string | null
  ip?: string | null
  user_agent?: string | null
  created_at: number
  expires_at: number
  /** True for the caller's own session. */
  current: boolean
}

export class ApiError extends Error {
  readonly status: number

  constructor(status: number, message: string) {
    super(message || `HTTP ${status}`)
    this.status = status
  }
}

async function send(path: string, init?: RequestInit): Promise<Response> {
  const res = await fetch(`/aperio/api${path}`, init)
  // The session middleware answers expired sessions with a redirect to the
  // login page; navigate there instead of trying to parse HTML as JSON. The
  // server's redirect parameter points at the API endpoint that happened to
  // hit the expiry — replace it with the dashboard page the user is actually
  // on, so a successful login lands back there instead of on raw JSON.
  if (res.redirected && new URL(res.url).pathname === '/aperio/auth') {
    const here = `${window.location.pathname}${window.location.search}`
    window.location.assign(`/aperio/auth?redirect=${encodeURIComponent(here)}`)
    throw new ApiError(401, 'session expired')
  }
  if (!res.ok) throw new ApiError(res.status, await res.text())
  return res
}

async function request<T>(path: string, init?: RequestInit): Promise<T> {
  return (await send(path, init)).json() as Promise<T>
}

/** Fire-and-forget mutation; the response body (if any) is ignored. */
async function mutate(path: string, init?: RequestInit): Promise<void> {
  await send(path, init)
}

function json(method: string, body: unknown): RequestInit {
  return {
    method,
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(body),
  }
}

/** A client-less static route (the `routes:` section): a hostname/path that
 * resolves to a server-produced redirect or fixed response, no client behind it. */
export interface TopoStaticRoute {
  hostname: string | null
  path: string | null
  action: 'redirect' | 'respond'
  target: string | null
  status: number
}

/** An experimental public TCP expose port. The shared key is never sent; only
 * whether a connected client currently serves it. */
export interface TopoExpose {
  port: number
  protocol: string
  served: boolean
  served_by: string | null
}

/** A hostname/path a token may bind but that no live client currently serves —
 * declared (granted) yet offline. */
export interface TopoOffline {
  bind: string
  kind: 'hostname' | 'path'
  token_name: string
}

/** The routing map: live tunnel clients plus the client-less routing the server
 * owns (static routes + expose ports; master organization only) and the
 * token-granted binds no client currently serves. */
export interface TopologyGraph {
  clients: ClientDetail[]
  routes: TopoStaticRoute[]
  exposes: TopoExpose[]
  offline: TopoOffline[]
}

export const api = {
  stats: () => request<ServerStats>('/stats'),
  uptime: () => request<UptimeEntry[]>('/uptime'),
  topology: () => request<TopologyGraph>('/topology'),
  statsHistory: (q: { unit?: string; count?: number; from?: string; to?: string }) => {
    const params = new URLSearchParams()
    if (q.from) {
      params.set('from', q.from)
      if (q.to) params.set('to', q.to)
    } else {
      if (q.unit) params.set('unit', q.unit)
      if (q.count) params.set('count', String(q.count))
    }
    return request<HistoryBucket[]>(`/stats/history?${params.toString()}`)
  },
  /** Public liveness probe (outside /api, no session needed). */
  health: async () => {
    const res = await fetch('/aperio/health')
    if (!res.ok) throw new ApiError(res.status, await res.text())
    return res.json() as Promise<{ version: string; protocol: number }>
  },
  logs: () => request<RequestLog[]>('/logs'),
  session: () => request<SessionInfo>('/session'),
  users: () => request<DashboardUser[]>('/users'),
  stageStats: () => request<RouteStageStats[]>('/stage-stats'),
  sessions: () => request<LiveSession[]>('/sessions'),
  revokeSession: (id: string) => mutate(`/sessions/${encodeURIComponent(id)}`, { method: 'DELETE' }),
  clearSessions: () => request<{ ended: number }>('/sessions', { method: 'DELETE' }),
  totpSetup: () =>
    request<{ secret: string; otpauth_url: string }>('/me/totp/setup', { method: 'POST' }),
  totpEnable: (code: string) =>
    request<{ recovery_codes: string[] }>('/me/totp/enable', json('POST', { code })),
  totpDisable: (code: string) => request<{ status: string }>('/me/totp', json('DELETE', { code })),
  totpAdminReset: (id: string) =>
    mutate(`/users/${encodeURIComponent(id)}/totp`, { method: 'DELETE' }),
  passkeys: () => request<PasskeyInfo[]>('/me/passkeys'),
  passkeyRegisterStart: () =>
    request<{ ceremony_id: string; challenge: { publicKey: never } }>(
      '/me/passkeys/register/start',
      { method: 'POST' },
    ),
  passkeyRegisterFinish: (payload: { ceremony_id: string; name?: string; credential: unknown
    usernameless?: boolean }) =>
    mutate('/me/passkeys/register/finish', json('POST', payload)),
  passkeyDelete: (id: string) =>
    mutate(`/me/passkeys/${encodeURIComponent(id)}`, { method: 'DELETE' }),
  createUser: (payload: { username: string; password: string; role: Role }) =>
    request<DashboardUser>('/users', json('POST', payload)),
  updateUser: (
    id: string,
    payload: { role?: Role; enabled?: boolean; password?: string },
  ) => mutate(`/users/${encodeURIComponent(id)}`, json('PUT', payload)),
  deleteUser: (id: string) => mutate(`/users/${encodeURIComponent(id)}`, { method: 'DELETE' }),
  cacheStats: () => request<CacheStats>('/cache/stats'),
  selfHealth: () => request<SelfHealth>('/self-health'),
  purgeCache: (payload: {
    hostname?: string
    path_prefix?: string
    surrogate_key?: string
  }) => request<{ removed: number }>('/cache/purge', json('POST', payload)),
  requestDetail: (id: string) => request<CapturedRequest>(`/requests/${encodeURIComponent(id)}`),
  replayRequest: (id: string) =>
    request<ReplayResult>(`/requests/${encodeURIComponent(id)}/replay`, { method: 'POST' }),
  overrideClient: (id: string, hostnameBind: string, pathBind: string) =>
    mutate(
      `/clients/${encodeURIComponent(id)}/override`,
      json('POST', { hostname_bind: hostnameBind, path_bind: pathBind }),
    ),
  setClientEnabled: (id: string, enabled: boolean) =>
    mutate(`/clients/${encodeURIComponent(id)}/enabled`, json('POST', { enabled })),
  tokens: () => request<TokenView[]>('/tokens'),
  createToken: (payload: TokenCreatePayload) =>
    request<{ token: string }>('/tokens', json('POST', payload)),
  updateToken: (id: string, payload: TokenUpdatePayload) =>
    mutate(`/tokens/${encodeURIComponent(id)}`, json('PUT', payload)),
  revokeToken: (id: string) => mutate(`/tokens/${encodeURIComponent(id)}`, { method: 'DELETE' }),
  adminKeys: () => request<AdminKeyView[]>('/admin-keys'),
  createAdminKey: (payload: AdminKeyCreatePayload) =>
    request<{ key: string }>('/admin-keys', json('POST', payload)),
  revokeAdminKey: (id: string) =>
    mutate(`/admin-keys/${encodeURIComponent(id)}`, { method: 'DELETE' }),
  webhooks: () => request<Webhook[]>('/webhooks'),
  createWebhook: (payload: {
    name: string
    url: string
    events: string[]
    secret?: string
    format?: string
  }) =>
    mutate('/webhooks', json('POST', payload)),
  deleteWebhook: (id: string) => mutate(`/webhooks/${encodeURIComponent(id)}`, { method: 'DELETE' }),
  webhookDeliveries: () => request<WebhookDelivery[]>('/webhooks/deliveries'),
  redeliverWebhook: (id: string) =>
    mutate(`/webhooks/deliveries/${encodeURIComponent(id)}/redeliver`, { method: 'POST' }),
  audit: () => request<AuditEvent[]>('/audit'),
  maintenance: () => request<string[]>('/maintenance'),
  settings: () => request<SettingsPayload>('/settings'),
  updateSettings: (overrides: SettingsOverrides) =>
    request<{ effective: SettingsValues }>('/settings', json('PUT', overrides)),
  createShareLink: (payload: { hostname: string; path?: string; ttl_seconds?: number }) =>
    request<{ id: string; url: string; token: string; expires_at: number | null }>(
      '/share',
      json('POST', payload),
    ),
  setMaintenance: (hostname: string, enabled: boolean) =>
    mutate('/maintenance', json('POST', { hostname, enabled })),
  orgs: () => request<Organization[]>('/orgs'),
  createOrg: (name: string) => request<{ id: string; name: string }>('/orgs', json('POST', { name })),
  deleteOrg: (id: string) => mutate(`/orgs/${encodeURIComponent(id)}`, { method: 'DELETE' }),
  setOrgQuota: (id: string, quota: OrgQuota) =>
    mutate(`/orgs/${encodeURIComponent(id)}/quota`, json('PUT', quota)),
  orgUsage: (id: string) => request<OrgUsage>(`/orgs/${encodeURIComponent(id)}/usage`),
  setOrgOidc: (id: string, oidc: OrgOidcPayload) =>
    request<{ id: string; configured: boolean }>(
      `/orgs/${encodeURIComponent(id)}/oidc`,
      json('PUT', oidc),
    ),
  selectOrg: (id: string) => request<{ selected: string }>('/orgs/select', json('POST', { id })),
}

/** Ends the dashboard session. Lives at /aperio/auth/logout (outside the /api
 *  namespace), so it bypasses the `send` helper's `/aperio/api` prefix. */
export async function logout(): Promise<void> {
  await fetch('/aperio/auth/logout', { method: 'POST' })
}
