/** Every audit event kind the server records, grouped for the filter.
 *
 * The audit API matches `event` **exactly**, not as a substring, which is why
 * this list exists at all: typing `login` into a free-text box returned zero
 * results even though `login_success` and `login_failed` were both in the log,
 * and an empty result on an audit screen reads as "that never happened". A
 * closed set of choices cannot be typed wrong. Partial matching is still
 * available in the search box, which does look inside the event name.
 *
 * Grouped because there are seventy of them and a flat list of seventy is a
 * scroll, not a choice. The groups follow what an operator is investigating,
 * not where the code lives.
 *
 * **This list is checked against the server's own sources by a test**
 * (`aperio-server/src/store/audit_tests.rs`): every `state.audit("...")` in
 * the server has to appear here, or the build fails. A hand-kept list would be
 * correct on the day it was written and wrong at the next release, and the
 * event missing from it would be the newest one, which is exactly the one
 * somebody is looking for.
 */
export interface AuditEventGroup {
  /** Untranslated group heading; the component passes it through `t`. */
  label: string
  events: string[]
}

export const AUDIT_EVENT_GROUPS: AuditEventGroup[] = [
  {
    label: 'Sign-in',
    events: [
      'login_success',
      'login_failed',
      'login_lockout',
      'oidc_login_success',
      'oidc_login_denied',
      'passkey_registered',
      'passkey_deleted',
      'totp_enabled',
      'totp_disabled',
      'totp_admin_reset',
      'session_revoked',
      'sessions_cleared',
    ],
  },
  {
    label: 'Tokens and keys',
    events: [
      'token_created',
      'token_updated',
      'token_revoked',
      'token_rotated',
      'token_refreshed',
      'token_expiring',
      'token_new_ip',
      'token_pin_mismatch',
      'canary_tripped',
      'admin_key_created',
      'admin_key_revoked',
      'share_created',
    ],
  },
  {
    label: 'Clients and tunnels',
    events: [
      'client_connected',
      'client_disconnected',
      'client_draining',
      'client_enabled',
      'client_disabled',
      'client_overrule',
      'tunnel_created',
      'tunnel_deleted',
      'tunnel_denied',
      'tcp_stream_opened',
      'udp_stream_opened',
      'expose_stream_opened',
    ],
  },
  {
    label: 'Users and organizations',
    events: [
      'user_created',
      'user_updated',
      'user_deleted',
      'org_created',
      'org_renamed',
      'org_deleted',
      'org_hostnames_set',
      'org_quota_updated',
      'org_oidc_updated',
    ],
  },
  {
    label: 'Configuration',
    events: [
      'config_reloaded',
      'settings_updated',
      'settings_override_dropped',
      'maintenance_on',
      'maintenance_off',
      'cache_purged',
    ],
  },
  {
    label: 'Data',
    events: [
      'export_created',
      'import_applied',
      'db_backup',
      'data_purged',
      'retention_pruned',
      'disk_pruned',
      'request_replayed',
    ],
  },
  {
    label: 'Alerts and scaling',
    events: [
      'alert_triggered',
      'alert_resolved',
      'disk_usage_warning',
      'scaling_requested',
      'scaling_failed',
      'scaling_disarmed',
    ],
  },
  {
    label: 'Webhooks and messages',
    events: [
      'webhook_created',
      'webhook_deleted',
      'webhook_tested',
      'webhook_refired',
      'webhook_redelivered',
      'message_published',
    ],
  },
]

/** Every kind, flat. Used by the parity test and by the "any event" reset. */
export const ALL_AUDIT_EVENTS: string[] = AUDIT_EVENT_GROUPS.flatMap((g) => g.events)
