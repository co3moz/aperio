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
  /**
   * A feature most deployments never turn on. The builder and the settings
   * pane list these after every other section, under one "Advanced features"
   * heading, so the settings everyone decides are not read past them on the
   * way. Nothing in one is hidden or turned off: it is ordering, not access.
   */
  advanced?: boolean
}

/** `aperio.yaml`, most consequential first. */
export const CLIENT_GROUPS: GroupSpec[] = [
  {
    title: 'Server connection',
    description: 'Which Aperio server this client dials, and how it identifies itself.',
    keys: ['server', 'name', 'client_id', 'client_id_file', 'device_key', 'device_key_file'],
  },
  {
    title: 'Services',
    description: 'Each entry is one exposed backend with its own bind and tuning.',
    keys: ['services', 'serve_spa', 'serve_404'],
  },
  {
    title: 'Service defaults',
    description:
      'What every service gets unless its own entry says otherwise. The environment and the command line write here too.',
    keys: ['defaults'],
  },
  {
    title: 'Logging',
    description: 'What this client writes and in which format.',
    keys: ['log_level', 'log_format'],
  },
  {
    title: 'Process',
    description: 'Files this one includes, the pid file, and how long a reload lets requests finish.',
    keys: ['include', 'pid_file', 'reload_drain'],
  },
  {
    title: 'Tunnels',
    description:
      'Private TCP/UDP services a peer client can bind locally, and the peers this client binds.',
    keys: ['tunnels', 'binds'],
    advanced: true,
  },
  {
    title: 'Messages',
    description:
      'Topics this client listens for, and where an application on the same machine attaches to send and receive them.',
    keys: ['subscribe', 'messages'],
    advanced: true,
  },
  {
    title: 'Telemetry bridge',
    description: 'Carrying OpenTelemetry exports from this machine to the server’s collector over the tunnel.',
    keys: ['otel_bridge'],
    advanced: true,
  },
  {
    title: 'Checks the server asks for',
    description: 'Whether this client answers a forward check the server’s own gate wrote, from this client’s network.',
    keys: ['allow_server_forward'],
    advanced: true,
  },
  {
    title: 'Capacity & pacing',
    description: 'How much work this client accepts and how fast the server may push to it.',
    keys: ['bandwidth', 'max_message_size'],
    advanced: true,
  },
  {
    title: 'Autoscaling',
    description: 'Asking for capacity when it is needed, and retiring when idle.',
    keys: ['scaling', 'idle_timeout'],
    advanced: true,
  },
]

/** The server's settings document, by section. Shared by the settings form
 *  and the builder's first-start document, so the two arrange the same
 *  sections the same way. */
export const SERVER_GROUPS: GroupSpec[] = [
  {
    title: 'Routing & load balancing',
    description: 'How a request finds a client, and which clients are eligible.',
    keys: ['routing'],
  },
  {
    title: 'Sites',
    description:
      'What the server does for a hostname by itself: answers, policy, fallbacks, pages and maintenance windows.',
    keys: ['sites'],
  },
  {
    title: 'Failover & retry',
    description: 'What happens when a client dies mid-request or answers with an error.',
    keys: ['resilience'],
  },
  {
    title: 'Limits & protection',
    description: 'Ceilings on bodies, concurrency, tunnels, and per-visitor rate.',
    keys: ['limits', 'gateway'],
  },
  {
    title: 'Caching',
    description: 'The server-side GET cache for services that opt in.',
    keys: ['cache'],
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
    title: 'Process & maintenance',
    description: 'How the server shuts down, and its scheduled backups.',
    keys: ['shutdown', 'backup'],
  },
  {
    title: 'Outbound & autoscaling',
    description: 'Where the server may call out to, and honouring client scaling blocks.',
    keys: ['outbound', 'scaling'],
    advanced: true,
  },
  {
    title: 'Edge integration',
    description: 'Publishing the live hostnames to a reverse proxy in front of this server.',
    keys: ['edge'],
    advanced: true,
  },
  {
    title: 'Flow control & compression',
    description: 'Stream flow control and tunnel compression. The defaults suit almost every deployment.',
    keys: ['advanced'],
    advanced: true,
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
