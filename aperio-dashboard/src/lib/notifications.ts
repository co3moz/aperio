/** Server events as the notification bell holds them.
 *
 * These are the same events that feed webhooks and the `$aperio/` topics,
 * pushed over the dashboard's SSE stream as `notification` frames. The event
 * *name* is deliberately not translated: it is the identifier an operator
 * subscribes a webhook to and filters the audit log by, so a bell that renamed
 * it per language would break the link between the three screens that talk
 * about the same thing.
 */
export interface ServerNotification {
  /** Stable per-session id, assigned on arrival (the wire carries none). */
  id: string
  /** Event name, e.g. `client_disconnected`. */
  event: string
  /** RFC 3339, from the server. */
  timestamp: string
  /** The event's own fields. */
  data: Record<string, unknown>
}

/** How an event reads at a glance, mirroring the webhook card colours. */
export type NotificationSeverity = 'good' | 'bad' | 'warn' | 'info'

const GOOD = new Set([
  'client_connected',
  'alert_resolved',
  'maintenance_off',
  'tunnel_created',
  'share_created',
  'token_created',
  'db_backup',
  'import_applied',
])
const BAD = new Set([
  'client_disconnected',
  'alert_triggered',
  'canary_tripped',
  'token_revoked',
  'token_pin_mismatch',
])
const WARN = new Set([
  'client_draining',
  'maintenance_on',
  'token_expiring',
  'token_new_ip',
  'org_usage',
  'disk_usage_warning',
])

/**
 * The event's nature, kept in step with the server's `event_hex` in
 * `store/webhooks.rs`: an event that is red on a Slack card should not be a
 * neutral line in the bell. An unknown event falls through to `info` rather
 * than guessing, which is what lets a new server event show up in an older
 * dashboard without looking like an alarm.
 */
export function severityOf(event: string): NotificationSeverity {
  if (GOOD.has(event)) return 'good'
  if (BAD.has(event)) return 'bad'
  if (WARN.has(event)) return 'warn'
  return 'info'
}

/** Events worth a badge on a backgrounded tab, rather than just a row. */
export function isUrgent(event: string): boolean {
  return severityOf(event) === 'bad' || severityOf(event) === 'warn'
}

// Fields listed first when an event carries several: what identifies the thing
// the event is about, before whatever else it happens to include.
const PREFERRED = ['name', 'client_id', 'hostname', 'token', 'kind', 'id', 'reason']

/**
 * The event's fields as one short line.
 *
 * Generic on purpose: every event has a different payload, and a per-event
 * template would be a list to forget to extend. Scalars only, since a nested
 * object rendered inline is noise, and at most three of them, since the line
 * has to stay a line.
 */
export function detailOf(data: Record<string, unknown>, max = 3): string {
  const scalar = Object.entries(data ?? {}).filter(
    ([, v]) => v !== null && v !== undefined && typeof v !== 'object',
  )
  const rank = (k: string) => {
    const i = PREFERRED.indexOf(k)
    return i === -1 ? PREFERRED.length : i
  }
  return scalar
    .sort((a, b) => rank(a[0]) - rank(b[0]))
    .slice(0, max)
    .map(([k, v]) => `${k}: ${String(v)}`)
    .join(' · ')
}
