import { ResetIcon } from '@radix-ui/react-icons'
import {
  Badge,
  Button,
  Callout,
  Card,
  Flex,
  Heading,
  IconButton,
  Select,
  Switch,
  Text,
  TextArea,
  TextField,
  Tooltip,
} from '@radix-ui/themes'
import { useEffect, useState } from 'react'
import { api, ApiError, type SettingsOverrides, type SettingsPayload } from '../lib/api'

type FieldKind = 'number' | 'boolean' | 'select' | 'text' | 'textarea'

interface FieldSpec {
  key: string
  label: string
  kind: FieldKind
  options?: string[]
  hint?: string
}

const FIELDS: FieldSpec[] = [
  { key: 'gateway_timeout_secs', label: 'Gateway timeout (s)', kind: 'number', hint: 'Seconds to wait for a client to (re)connect before failing a request' },
  { key: 'gateway_response_timeout_secs', label: 'Response timeout (s)', kind: 'number', hint: 'Seconds to wait for a client to answer a dispatched request' },
  { key: 'max_body_size', label: 'Max request body (bytes)', kind: 'number' },
  { key: 'max_tunnels', label: 'Max tunnel clients', kind: 'number' },
  { key: 'client_down_threshold_secs', label: 'Client down threshold (s)', kind: 'number', hint: 'Missed-heartbeat window before a client leaves routing' },
  { key: 'require_hostname_bind', label: 'Require hostname bind', kind: 'boolean', hint: 'Strict multi-tenant mode: unbound clients never receive traffic' },
  { key: 'lb_strategy', label: 'Load balancing', kind: 'select', options: ['round-robin', 'primary-standby', 'sticky'] },
  { key: 'failover_mode', label: 'In-flight failover', kind: 'select', options: ['fail', 'retry', 'wait', 'retry-wait'] },
  { key: 'failover_max_jumps', label: 'Failover max jumps', kind: 'number' },
  { key: 'failover_window_secs', label: 'Failover window (s)', kind: 'number' },
  { key: 'failover_all_methods', label: 'Failover non-idempotent methods', kind: 'boolean', hint: 'POST/PATCH may reach a backend twice when enabled' },
  { key: 'ip_limit_max', label: 'IP rate limit burst', kind: 'number' },
  { key: 'ip_limit_refill', label: 'IP rate limit refill (req/s)', kind: 'number' },
  { key: 'tunnel_compression', label: 'Tunnel compression', kind: 'boolean', hint: 'Enabling is offered to connected clients immediately; disabling applies to new connections' },
  { key: 'random_subdomain_suffix', label: 'Random subdomain suffix', kind: 'text', hint: 'e.g. example.com, *.example.com or *-test.example.com — * is replaced with a random label; empty = disabled' },
  { key: 'auth_credentials', label: 'Visitor password', kind: 'text', hint: 'user:password put in front of all proxied traffic; empty = disabled' },
  { key: 'custom_504_page', label: 'Custom 504 page (HTML)', kind: 'textarea' },
  { key: 'custom_503_page', label: 'Custom 503 maintenance page (HTML)', kind: 'textarea' },
]

/**
 * Dashboard-editable server settings. Environment variables provide the
 * defaults; edits become overrides that apply live and persist in
 * `<data_dir>/settings.json`. The master token, HOST/PORT, proxy trust and
 * OIDC remain env-only.
 */
export function SettingsSection() {
  const [data, setData] = useState<SettingsPayload | null>(null)
  const [overrides, setOverrides] = useState<SettingsOverrides>({})
  const [dirty, setDirty] = useState(false)
  const [busy, setBusy] = useState(false)
  const [message, setMessage] = useState<{ ok: boolean; text: string } | null>(null)

  const load = () => {
    api
      .settings()
      .then((payload) => {
        setData(payload)
        setOverrides({ ...payload.overrides })
        setDirty(false)
      })
      .catch(() => {})
  }
  useEffect(load, [])

  const setField = (key: string, value: string | number | boolean) => {
    setOverrides((o) => ({ ...o, [key]: value }))
    setDirty(true)
    setMessage(null)
  }
  const resetField = (key: string) => {
    setOverrides((o) => {
      const next = { ...o }
      delete next[key]
      return next
    })
    setDirty(true)
    setMessage(null)
  }

  const save = async () => {
    setBusy(true)
    setMessage(null)
    try {
      await api.updateSettings(overrides)
      setMessage({ ok: true, text: 'Settings applied and persisted.' })
      load()
    } catch (e) {
      setMessage({ ok: false, text: e instanceof ApiError ? e.message : String(e) })
    } finally {
      setBusy(false)
    }
  }

  if (!data) return null

  const valueOf = (key: string) => overrides[key] ?? data.defaults[key]

  const control = (f: FieldSpec) => {
    const value = valueOf(f.key)
    switch (f.kind) {
      case 'boolean':
        return (
          <Switch checked={Boolean(value)} onCheckedChange={(v) => setField(f.key, v)} />
        )
      case 'select':
        return (
          <Select.Root value={String(value ?? '')} onValueChange={(v) => setField(f.key, v)}>
            <Select.Trigger />
            <Select.Content>
              {(f.options ?? []).map((o) => (
                <Select.Item key={o} value={o}>
                  {o}
                </Select.Item>
              ))}
            </Select.Content>
          </Select.Root>
        )
      case 'number':
        return (
          <TextField.Root
            type="number"
            value={String(value ?? '')}
            onChange={(e) => {
              const n = Number(e.target.value)
              if (Number.isFinite(n)) setField(f.key, n)
            }}
            style={{ width: 140 }}
          />
        )
      case 'text':
        return (
          <TextField.Root
            value={String(value ?? '')}
            onChange={(e) => setField(f.key, e.target.value)}
            style={{ width: 280 }}
          />
        )
      case 'textarea':
        return (
          <TextArea
            value={String(value ?? '')}
            onChange={(e) => setField(f.key, e.target.value)}
            rows={3}
            style={{ width: '100%', fontFamily: 'var(--code-font-family)', fontSize: 12 }}
          />
        )
    }
  }

  return (
    <Flex direction="column" gap="3">
      <Flex justify="between" align="center">
        <Heading size="4">Server Settings</Heading>
        <Button onClick={save} loading={busy} disabled={!dirty}>
          Save & apply
        </Button>
      </Flex>
      <Card size="3">
        <Flex direction="column" gap="3">
          <Text size="1" color="gray">
            Environment variables provide the defaults; edits below become overrides that apply
            immediately and persist across restarts ({'<data_dir>'}/settings.json). The master
            token, HOST/PORT, proxy trust and OIDC stay env-only.
          </Text>
          {message && (
            <Callout.Root color={message.ok ? 'green' : 'red'} size="1">
              <Callout.Text>{message.text}</Callout.Text>
            </Callout.Root>
          )}
          {FIELDS.map((f) => {
            const overridden = overrides[f.key] !== undefined && overrides[f.key] !== null
            return (
              <Flex key={f.key} align="center" gap="3" wrap="wrap">
                <Flex direction="column" style={{ width: 260, flexShrink: 0 }}>
                  <Text size="2">{f.label}</Text>
                  {f.hint && (
                    <Text size="1" color="gray">
                      {f.hint}
                    </Text>
                  )}
                </Flex>
                <div style={{ flex: 1, minWidth: 160 }}>{control(f)}</div>
                {overridden ? (
                  <Flex align="center" gap="2">
                    <Badge color="amber" size="1">
                      override
                    </Badge>
                    <Tooltip content={`Reset to env default (${JSON.stringify(data.defaults[f.key])})`}>
                      <IconButton size="1" variant="ghost" onClick={() => resetField(f.key)}>
                        <ResetIcon />
                      </IconButton>
                    </Tooltip>
                  </Flex>
                ) : (
                  <Text size="1" color="gray">
                    default
                  </Text>
                )}
              </Flex>
            )
          })}
        </Flex>
      </Card>
    </Flex>
  )
}
