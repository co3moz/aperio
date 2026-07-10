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
  protocol: number | null
  protocol_mismatch: boolean
  backend_healthy: boolean
  priority: number
  bandwidth_bps: number | null
  healthy: boolean
  draining: boolean
  enabled: boolean
  instance_id: string | null
  instance_id_shared: boolean
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
}

export interface TokenUpdatePayload {
  hostnames: string[]
  paths: string[]
  allowed_ips: string[]
  ttl_seconds?: number
  max_rps?: number
  daily_max_bytes?: number
  allow_public?: boolean
}

export interface Webhook {
  id: string
  name: string
  url: string
  events: string[]
  enabled: boolean
  created_at: number
  /** True when deliveries are HMAC-signed (the secret itself is never returned). */
  signed: boolean
}

export interface AuditEvent {
  ts: number
  timestamp: string
  event: string
  actor_ip: string
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
}

export interface DashboardUser {
  id: string
  username: string
  role: Role
  created_at: number
  enabled: boolean
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

export const api = {
  stats: () => request<ServerStats>('/stats'),
  /** Public liveness probe (outside /api, no session needed). */
  health: async () => {
    const res = await fetch('/aperio/health')
    if (!res.ok) throw new ApiError(res.status, await res.text())
    return res.json() as Promise<{ version: string; protocol: number }>
  },
  logs: () => request<RequestLog[]>('/logs'),
  session: () => request<SessionInfo>('/session'),
  users: () => request<DashboardUser[]>('/users'),
  createUser: (payload: { username: string; password: string; role: Role }) =>
    request<DashboardUser>('/users', json('POST', payload)),
  updateUser: (
    id: string,
    payload: { role?: Role; enabled?: boolean; password?: string },
  ) => mutate(`/users/${encodeURIComponent(id)}`, json('PUT', payload)),
  deleteUser: (id: string) => mutate(`/users/${encodeURIComponent(id)}`, { method: 'DELETE' }),
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
  webhooks: () => request<Webhook[]>('/webhooks'),
  createWebhook: (payload: { name: string; url: string; events: string[]; secret?: string }) =>
    mutate('/webhooks', json('POST', payload)),
  deleteWebhook: (id: string) => mutate(`/webhooks/${encodeURIComponent(id)}`, { method: 'DELETE' }),
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
}

/** Ends the dashboard session. Lives at /aperio/auth/logout (outside the /api
 *  namespace), so it bypasses the `send` helper's `/aperio/api` prefix. */
export async function logout(): Promise<void> {
  await fetch('/aperio/auth/logout', { method: 'POST' })
}
