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
  protocol: number | null
  protocol_mismatch: boolean
  backend_healthy: boolean
  priority: number
  bandwidth_bps: number | null
  healthy: boolean
  draining: boolean
  enabled: boolean
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
}

export interface TokenCreatePayload {
  name: string
  hostnames: string[]
  paths: string[]
  allowed_ips: string[]
  ttl_seconds?: number
  max_rps?: number
  daily_max_bytes?: number
}

export interface TokenUpdatePayload {
  hostnames: string[]
  paths: string[]
  allowed_ips: string[]
  ttl_seconds?: number
  max_rps?: number
  daily_max_bytes?: number
}

export interface Webhook {
  id: string
  name: string
  url: string
  events: string[]
  enabled: boolean
  created_at: number
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

export interface SettingsPayload {
  effective: SettingsValues
  defaults: SettingsValues
  overrides: SettingsOverrides
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
  // login page; navigate there instead of trying to parse HTML as JSON.
  if (res.redirected && new URL(res.url).pathname === '/aperio/auth') {
    window.location.assign(res.url)
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
  logs: () => request<RequestLog[]>('/logs'),
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
  createWebhook: (payload: { name: string; url: string; events: string[] }) =>
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
