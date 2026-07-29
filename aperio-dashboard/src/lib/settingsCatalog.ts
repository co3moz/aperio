/**
 * What the dashboard-editable server settings are, and how each one is
 * entered.
 *
 * Its own module because two very different things need the same list: the
 * settings form renders it, and the command palette searches it so a setting
 * can be found by name from anywhere without knowing which group it lives in.
 */
export type FieldKind = 'number' | 'bytes' | 'boolean' | 'select' | 'text' | 'textarea'

export interface FieldSpec {
  key: string
  label: string
  kind: FieldKind
  options?: string[]
  /** What each option actually makes the server do, shown under the picker
   *  once it is chosen. A list of words is not a choice: `sticky` and
   *  `round-robin` name themselves, and say nothing about what happens to a
   *  request, which is the thing being decided. */
  optionHints?: Record<string, string>
  hint?: string
  /** Smallest accepted value; the server rejects anything below it, so the
   *  field says so rather than letting the save come back a 400. */
  min?: number
}

export interface GroupSpec {
  title: string
  description: string
  fields: FieldSpec[]
}

// Related settings live together; unrelated ones get their own card.
export const GROUPS: GroupSpec[] = [
  {
    title: 'Gateway & Requests',
    description: 'Timeouts and size limits applied to every proxied request.',
    fields: [
      { key: 'gateway_timeout_secs', label: 'Gateway timeout (s)', kind: 'number', hint: 'Wait for a client to (re)connect before failing a request' },
      { key: 'gateway_response_timeout_secs', label: 'Response timeout (s)', kind: 'number', hint: 'Wait for a client to answer a dispatched request' },
      { key: 'max_body_size', label: 'Max request body', kind: 'bytes', hint: 'Requests with larger bodies are rejected up front' },
    ],
  },
  {
    title: 'Capacity & Health',
    description: 'How many clients may connect, how much runs at once, and when a client counts as down.',
    fields: [
      { key: 'max_tunnels', label: 'Max tunnel clients', kind: 'number', hint: 'Connection attempts beyond this are refused' },
      { key: 'max_concurrent_requests', label: 'Max concurrent requests', kind: 'number', hint: 'In-flight proxied requests; beyond it visitors get 429' },
      { key: 'client_down_threshold_secs', label: 'Client down threshold (s)', kind: 'number', hint: 'Missed-heartbeat window before a client leaves routing' },
    ],
  },
  {
    title: 'Routing & Failover',
    description: 'How requests pick a client and what happens when one is lost mid-request.',
    fields: [
      {
        key: 'lb_strategy',
        label: 'Load balancing',
        kind: 'select',
        options: ['round-robin', 'primary-standby', 'sticky'],
        hint: 'Strategy for picking a client from the routed pool',
        optionHints: {
          'round-robin':
            'Every healthy client of the route takes requests in turn, evenly. The default, and what you want when the clients are interchangeable.',
          'primary-standby':
            'Only the clients on the lowest priority tier receive traffic; a higher tier takes over when every client above it is unhealthy, draining or gone, and hands back when one returns. Tiers come from each client’s priority (0 = primary).',
          sticky:
            'A visitor keeps the client that first served them, for as long as it stays healthy. For backends holding per-visitor state in memory; the pool spreads by visitor rather than by request.',
        },
      },
      { key: 'require_hostname_bind', label: 'Require hostname bind', kind: 'boolean', hint: 'Strict multi-tenant mode: unbound clients never receive traffic' },
      {
        key: 'failover_mode',
        label: 'In-flight failover',
        kind: 'select',
        options: ['fail', 'retry', 'wait', 'retry-wait'],
        hint: 'Reaction when the serving client drops mid-request',
        optionHints: {
          fail: 'The visitor gets the error. Nothing is retried, which is the only safe answer if a request may not run twice.',
          retry: 'Re-dispatch to another healthy client of the same route, if there is one. Nothing waits.',
          wait: 'Hold the request while the same client reconnects, up to the failover window. For a single-client route, where there is nothing to fail over to.',
          'retry-wait':
            'Try another client first; if the route has none, wait for one to come back. The most forgiving, and the slowest to give up.',
        },
      },
      { key: 'failover_max_jumps', label: 'Failover max jumps', kind: 'number', hint: 'Re-dispatch attempts per request' },
      { key: 'failover_window_secs', label: 'Failover window (s)', kind: 'number', hint: 'Total time budget across all jumps' },
      { key: 'failover_all_methods', label: 'Failover non-idempotent methods', kind: 'boolean', hint: 'POST/PATCH may reach a backend twice when enabled' },
    ],
  },
  {
    title: 'Rate Limiting',
    description: 'Per-visitor-IP token bucket for proxied requests.',
    fields: [
      { key: 'ip_limit_max', label: 'Burst size', kind: 'number', hint: 'Requests a single IP may fire at once' },
      { key: 'ip_limit_refill', label: 'Refill rate (req/s)', kind: 'number', hint: 'Sustained requests per second per IP' },
    ],
  },
  {
    title: 'Tunnels & Domains',
    description: 'Behavior of the tunnel links and automatic hostnames.',
    fields: [
      { key: 'tunnel_compression', label: 'Tunnel compression', kind: 'boolean', hint: 'Enabling is offered to connected clients immediately; disabling applies to new connections' },
      { key: 'random_subdomain_suffix', label: 'Random subdomain pattern', kind: 'text', hint: 'e.g. example.com, *.example.com or *-test.example.com, * becomes a random label; empty = disabled' },
      { key: 'preview_noindex', label: 'Noindex preview hosts', kind: 'boolean', hint: 'Random-subdomain services answer with X-Robots-Tag: noindex and a disallow-all robots.txt' },
    ],
  },
  {
    title: 'Caching',
    description: 'Server-side response cache for services that opt in with cache: true.',
    fields: [
      { key: 'cache_enabled', label: 'Response cache', kind: 'boolean', hint: 'Cache-Control-driven GET cache; disabling clears stored entries' },
      { key: 'cache_max_bytes', label: 'Cache budget', kind: 'bytes', hint: 'Total memory for cached responses; entries closest to expiry are evicted first' },
      { key: 'cache_max_stale', label: 'Serve-stale window (s)', kind: 'number', hint: 'How long an expired entry may still answer while a resilient service has no healthy client; 0 = off' },
    ],
  },
  {
    title: 'Stream Flow Control',
    description:
      'How much of a slow visitor’s download the server buffers before asking the client to pause producing it.',
    fields: [
      { key: 'stream_pause_bytes', min: 1, label: 'Pause above', kind: 'bytes', hint: 'Backlog at which the producing client is told to stop reading that stream’s source' },
      { key: 'stream_resume_bytes', min: 1, label: 'Resume below', kind: 'bytes', hint: 'Backlog at which a paused producer carries on; kept well under the pause mark so the pair cannot flap' },
      { key: 'stream_backlog_limit', min: 1, label: 'Hard backlog cap', kind: 'bytes', hint: 'A stream whose producer cannot be paused (an older client) is dropped past this' },
    ],
  },
  {
    title: 'Security & Audit',
    description: 'Login brute-force protection and audit log rotation.',
    fields: [
      { key: 'login_lockout_threshold', label: 'Login lockout threshold', kind: 'number', hint: 'Consecutive failures per IP before a lockout starts' },
      { key: 'login_lockout_secs', label: 'Login lockout base (s)', kind: 'number', hint: 'First lockout duration; doubles per repeat offense' },
      { key: 'audit_max_size', label: 'Audit rotation size', kind: 'bytes', hint: 'audit.jsonl rotates past this size; 0 = never rotate' },
      { key: 'audit_max_files', label: 'Audit generations kept', kind: 'number', hint: 'Rotated audit.jsonl.N files to keep; oldest is dropped' },
    ],
  },
  {
    title: 'Visitor Experience',
    description: 'What visitors see in front of and around the proxied services.',
    fields: [
      { key: 'ui_language', label: 'Default UI language', kind: 'select', options: ['en', 'de', 'es', 'fr', 'tr', 'ru', 'zh', 'ja'], hint: 'Dashboard/login language for visitors whose browser language is unsupported' },
      { key: 'auth_credentials', label: 'Visitor password', kind: 'text', hint: 'user:password gate in front of all proxied traffic; empty = disabled' },
      { key: 'custom_504_page', label: 'Custom 504 page (HTML)', kind: 'textarea', hint: 'Shown when no client answers in time' },
      { key: 'custom_503_page', label: 'Custom 503 maintenance page (HTML)', kind: 'textarea', hint: 'Shown for hostnames in maintenance mode' },
    ],
  },
]

/** Every field, with the group it belongs to, what a search walks. */
export const SETTING_FIELDS: { field: FieldSpec; group: GroupSpec }[] = GROUPS.flatMap((group) =>
  group.fields.map((field) => ({ field, group })),
)

/** The DOM id the settings form gives a field, so a link can scroll to it. */
export function settingAnchor(key: string): string {
  return `setting-${key}`
}

