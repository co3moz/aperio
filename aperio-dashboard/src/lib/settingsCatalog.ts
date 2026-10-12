/**
 * The server settings people look up by name, with the words the dashboard
 * uses for them.
 *
 * The settings form renders every setting the schema describes; this list is
 * the part that cannot be derived. It gives the common settings a translated
 * label and a one-line hint, explains what each option of a choice actually
 * does, and is what the command palette searches, so a setting can be found by
 * name from anywhere without knowing which section it lives in.
 */

export interface FieldSpec {
  /** The setting's dotted path, as the server writes it (`gateway.timeout`). */
  key: string
  label: string
  /** What each option actually makes the server do, shown under the picker
   *  once it is chosen. A list of words is not a choice: `sticky` and
   *  `round-robin` name themselves, and say nothing about what happens to a
   *  request, which is the thing being decided. */
  optionHints?: Record<string, string>
  hint?: string
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
      { key: 'gateway.timeout', label: 'Gateway timeout', hint: 'Wait for a client to (re)connect before failing a request' },
      { key: 'gateway.response_timeout', label: 'Response timeout', hint: 'Wait for a client to answer a dispatched request' },
      { key: 'limits.request_body', label: 'Max request body', hint: 'Requests with larger bodies are rejected up front' },
    ],
  },
  {
    title: 'Capacity & Health',
    description: 'How many clients may connect, how much runs at once, and when a client counts as down.',
    fields: [
      { key: 'limits.tunnels', label: 'Max tunnel clients', hint: 'Connection attempts beyond this are refused' },
      { key: 'limits.concurrent_requests', label: 'Max concurrent requests', hint: 'In-flight proxied requests; beyond it visitors get 429' },
      { key: 'gateway.client_down_threshold', label: 'Client down threshold', hint: 'Missed-heartbeat window before a client leaves routing' },
    ],
  },
  {
    title: 'Routing & Failover',
    description: 'How requests pick a client and what happens when one is lost mid-request.',
    fields: [
      {
        key: 'routing.lb_strategy',
        label: 'Load balancing',
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
      { key: 'routing.require_hostname_bind', label: 'Require hostname bind', hint: 'Strict multi-tenant mode: unbound clients never receive traffic' },
      {
        key: 'failover.mode',
        label: 'In-flight failover',
        hint: 'Reaction when the serving client drops mid-request',
        optionHints: {
          fail: 'The visitor gets the error. Nothing is retried, which is the only safe answer if a request may not run twice.',
          retry: 'Re-dispatch to another healthy client of the same route, if there is one. Nothing waits.',
          wait: 'Hold the request while the same client reconnects, up to the failover window. For a single-client route, where there is nothing to fail over to.',
          'retry-wait':
            'Try another client first; if the route has none, wait for one to come back. The most forgiving, and the slowest to give up.',
        },
      },
      { key: 'failover.max_jumps', label: 'Failover max jumps', hint: 'Re-dispatch attempts per request' },
      { key: 'failover.window', label: 'Failover window', hint: 'Total time budget across all jumps' },
      { key: 'failover.all_methods', label: 'Failover non-idempotent methods', hint: 'POST/PATCH may reach a backend twice when enabled' },
    ],
  },
  {
    title: 'Rate Limiting',
    description: 'Per-visitor-IP token bucket for proxied requests.',
    fields: [
      { key: 'limits.ip_rate.burst', label: 'Burst size', hint: 'Requests a single IP may fire at once' },
      { key: 'limits.ip_rate.per_second', label: 'Refill rate (req/s)', hint: 'Sustained requests per second per IP' },
    ],
  },
  {
    title: 'Tunnels & Domains',
    description: 'Behavior of the tunnel links and automatic hostnames.',
    fields: [
      { key: 'tunnel.compression', label: 'Tunnel compression', hint: 'Enabling is offered to connected clients immediately; disabling applies to new connections' },
      { key: 'routing.random_subdomain', label: 'Random subdomain pattern', hint: 'e.g. example.com, *.example.com or *-test.example.com, * becomes a random label; empty = disabled' },
      { key: 'routing.preview_noindex', label: 'Noindex preview hosts', hint: 'Random-subdomain services answer with X-Robots-Tag: noindex and a disallow-all robots.txt' },
    ],
  },
  {
    title: 'Caching',
    description: 'Server-side response cache for services that opt in with cache: true.',
    fields: [
      { key: 'cache.enabled', label: 'Response cache', hint: 'Cache-Control-driven GET cache; disabling clears stored entries' },
      { key: 'cache.size', label: 'Cache budget', hint: 'Total memory for cached responses; entries closest to expiry are evicted first' },
      { key: 'cache.max_stale', label: 'Serve-stale window', hint: 'How long an expired entry may still answer while a resilient service has no healthy client; 0 = off' },
    ],
  },
  {
    title: 'Stream Flow Control',
    description:
      'How much of a slow visitor’s download the server buffers before asking the client to pause producing it.',
    fields: [
      { key: 'stream.pause_at', label: 'Pause above', hint: 'Backlog at which the producing client is told to stop reading that stream’s source' },
      { key: 'stream.resume_at', label: 'Resume below', hint: 'Backlog at which a paused producer carries on; kept well under the pause mark so the pair cannot flap' },
      { key: 'stream.backlog_limit', label: 'Hard backlog cap', hint: 'A stream whose producer cannot be paused (an older client) is dropped past this' },
    ],
  },
  {
    title: 'Security & Audit',
    description: 'Login brute-force protection and audit log rotation.',
    fields: [
      { key: 'security.login_lockout.threshold', label: 'Login lockout threshold', hint: 'Consecutive failures per IP before a lockout starts' },
      { key: 'security.login_lockout.duration', label: 'Login lockout base', hint: 'First lockout duration; doubles per repeat offense' },
      { key: 'audit.max_size', label: 'Audit rotation size', hint: 'audit.jsonl rotates past this size; 0 = never rotate' },
      { key: 'audit.max_files', label: 'Audit generations kept', hint: 'Rotated audit.jsonl.N files to keep; oldest is dropped' },
    ],
  },
  {
    title: 'Visitor Experience',
    description: 'What visitors see in front of and around the proxied services.',
    fields: [
      { key: 'dashboard.ui_language', label: 'Default UI language', hint: 'Dashboard/login language for visitors whose browser language is unsupported' },
      { key: 'access', label: 'Default access', hint: 'deny, public, or a gate in front of every route nothing else declares' },
      { key: 'sites', label: 'Sites', hint: 'What the server does for a hostname by itself: answers, policy, fallbacks, pages and maintenance windows.' },
    ],
  },
]

/** Every field, with the group it belongs to, what a search walks. */
export const SETTING_FIELDS: { field: FieldSpec; group: GroupSpec }[] = GROUPS.flatMap((group) =>
  group.fields.map((field) => ({ field, group })),
)

/** The catalogue's entry for a path, when it has one. */
export function catalogEntry(path: string): FieldSpec | undefined {
  return SETTING_FIELDS.find(({ field }) => field.key === path)?.field
}

/** The DOM id the settings form gives a field, so a link can scroll to it. */
export function settingAnchor(key: string): string {
  return `setting-${key.replaceAll('.', '-')}`
}
