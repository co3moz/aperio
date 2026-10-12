/**
 * How the config builder arranges the settings it finds in the schema.
 *
 * The schema knows every key but says nothing about which belong together, so
 * the grouping lives here: an ordered list of sections, each naming the keys
 * it claims. Anything the lists miss lands in a final catch-all rather than
 * disappearing, which is what keeps the form honest when a new setting is
 * added to the schema and nobody remembers to file it here.
 */

export interface GroupSpec {
  /** Section heading. */
  title: string
  /** One line on what the section is for. */
  description: string
  /** Top-level keys this section claims, in the order they should appear. */
  keys: string[]
}

/** `aperio.yaml`, most consequential first. */
export const CLIENT_GROUPS: GroupSpec[] = [
  {
    title: 'Server connection',
    description: 'Which Aperio server this client dials, and how it identifies itself.',
    keys: ['server', 'token', 'version', 'client_id', 'ip_family', 'device_key', 'device_key_file'],
  },
  {
    title: 'Services',
    description: 'Each entry is one exposed backend with its own bind and tuning.',
    keys: ['services'],
  },
  {
    title: 'Tunnels',
    description:
      'Private TCP/UDP services a peer client can bind locally, and the peers this client binds.',
    keys: ['tunnels', 'bind-tunnels'],
  },
  {
    title: 'Messages',
    description:
      'Topics this client listens for, and where an application on the same machine attaches to send and receive them.',
    keys: ['subscribe', 'messages_listen', 'messages_mqtt_listen'],
  },
  {
    title: 'Service defaults',
    description:
      'Applied to every entry under services:, and overridable per entry. The keys that name one backend at the top level are deprecated here, they only ever worked without a services: list, and appear only for a file that already writes them.',
    keys: [
      'target',
      'serve',
      'hostname',
      'path',
      'tcp_target',
      'serve_spa',
      'serve_404',
      'trim_bind',
      'pass_hostname',
    ],
  },
  {
    title: 'Access control',
    description: 'Who may reach the exposed service.',
    keys: ['public', 'auth', 'allowed_ips', 'denied'],
  },
  {
    title: 'Capacity & pacing',
    description: 'How much work this client accepts and how fast the server may push to it.',
    keys: [
      'max_concurrent',
      'connections',
      'priority',
      'bandwidth',
      'timeout',
      'response_timeout',
      'max_request_body',
      'max_response_body',
      'max_message_size',
      'max_redirects',
    ],
  },
  {
    title: 'Backend health',
    description:
      'Probing the backend so an unhealthy one leaves the routing pool without dropping the tunnel. Set here it applies to every entry under services:, and any entry may override it.',
    keys: [
      'health',
      'target_health',
      'health_interval',
      'health_timeout',
      'health_threshold',
      'wait_for_backend',
    ],
  },
  {
    title: 'Autoscaling',
    description: 'Asking for capacity when it is needed, and retiring when idle.',
    keys: ['scaling', 'idle_timeout'],
  },
  {
    title: 'Response handling',
    description: 'Caching, resilience, and the headers this client rewrites.',
    keys: ['cache', 'resilience', 'webhook_inbox', 'headers', 'security_headers'],
  },
  {
    title: 'Logging',
    description: 'What this client writes and in which format.',
    keys: ['log_level', 'log_format'],
  },
]

/** The server's settings document, by section. Shared by the settings form
 *  and the builder's first-start document, so the two arrange the same
 *  sections the same way. */
export const SERVER_GROUPS: GroupSpec[] = [
  {
    title: 'Routing & load balancing',
    description: 'How a request finds a client, and which clients are eligible.',
    keys: ['routing', 'outlier', 'routes', 'fallbacks'],
  },
  {
    title: 'Failover & retry',
    description: 'What happens when a client dies mid-request or answers with an error.',
    keys: ['failover'],
  },
  {
    title: 'Limits & protection',
    description: 'Ceilings on bodies, concurrency, tunnels, and per-visitor rate.',
    keys: ['limits', 'gateway', 'rate_limits', 'waf'],
  },
  {
    title: 'Caching',
    description: 'The server-side GET cache for services that opt in.',
    keys: ['cache'],
  },
  {
    title: 'Stream flow control',
    description: 'How much of a slow visitor’s download is buffered before the client pauses.',
    keys: ['stream', 'tunnel'],
  },
  {
    title: 'Authentication & dashboard',
    description: 'Who may log in, how, and what the dashboard offers.',
    keys: ['access', 'dashboard', 'oidc', 'security'],
  },
  {
    title: 'Proxy trust',
    description: 'How the real client IP is resolved behind a proxy or CDN.',
    keys: ['proxy'],
  },
  {
    title: 'Observability',
    description: 'Metrics, tracing, logs, alerting, and how long records are kept.',
    keys: [
      'metrics',
      'otel',
      'logging',
      'inspector',
      'request_id',
      'identity_headers',
      'alerts',
      'audit',
      'retention',
      'uptime',
      'webhooks',
    ],
  },
  {
    title: 'Outbound & autoscaling',
    description: 'Where the server may call out to, and honouring client scaling blocks.',
    keys: ['outbound', 'scaling'],
  },
  {
    title: 'Edge integration',
    description: 'Publishing the live hostnames to a reverse proxy in front of this server.',
    keys: ['edge'],
  },
  {
    title: 'Pages & headers',
    description: 'What visitors see on an error, and server-wide header rewriting.',
    keys: ['pages', 'error_pages', 'maintenance_windows', 'headers'],
  },
  {
    title: 'Process & maintenance',
    description: 'How the server shuts down, and its scheduled backups.',
    keys: ['shutdown', 'backup'],
  },
]

/**
 * How early a key has to be decided when describing a deployment.
 *
 * This is not about risk. It is the order someone actually thinks in: you
 * cannot say anything useful about a service before you have said *what* it is
 * and *where* it answers, so `target` and `hostname` come before the timeout
 * that shapes them. Tier 0 is rendered inline under its section; everything
 * else goes behind a nested "More settings" accordion, which is what keeps a
 * section with thirty keys readable.
 *
 * Applies at any depth, the same table orders the fields of a `services:`
 * entry, where schema order is alphabetical and therefore useless.
 */
export const ESSENTIAL_KEYS: string[] = [
  // Identity of the thing being configured.
  'name',
  'server',
  'url',
  'token',
  'version',
  // What it is.
  'target',
  'serve',
  'services',
  'tunnels',
  'protocol',
  'topic',
  // Where it answers.
  'hostname',
  'path',
  'port',
  'host',
  // Block-level switches: without these the rest of the block is inert.
  'enabled',
  'mode',
  'endpoint',
  'issuer',
  'client_id',
  'client_secret',
  'key',
]

/** Rank within tier 0; lower sorts first. Unlisted keys keep group order. */
const ESSENTIAL_ORDER = new Map(ESSENTIAL_KEYS.map((k, i) => [k, i]))

/** True when a key is one of the first things to decide. */
export function isEssential(key: string): boolean {
  return ESSENTIAL_ORDER.has(key)
}

/** Sort key for a field, so essentials lead in a sensible order. */
export function essentialRank(key: string): number {
  return ESSENTIAL_ORDER.get(key) ?? Number.MAX_SAFE_INTEGER
}
