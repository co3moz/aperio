import { RotateCcwIcon } from 'lucide-react'
import { useEffect, useState } from 'react'
import { SectionHeader } from './shared'
import { TintBadge } from './badges'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import { Input } from '@/components/ui/input'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { Spinner } from '@/components/ui/spinner'
import { Switch } from '@/components/ui/switch'
import { Textarea } from '@/components/ui/textarea'
import { Tooltip, TooltipContent, TooltipTrigger } from '@/components/ui/tooltip'
import { api, ApiError, type SettingsOverrides, type SettingsPayload } from '@/lib/api'
import { cn } from '@/lib/utils'

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
        return <Switch checked={Boolean(value)} onCheckedChange={(v) => setField(f.key, v)} />
      case 'select':
        return (
          <Select value={String(value ?? '')} onValueChange={(v) => setField(f.key, v as string)}>
            <SelectTrigger className="w-44">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              {(f.options ?? []).map((o) => (
                <SelectItem key={o} value={o}>
                  {o}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        )
      case 'number':
        return (
          <Input
            type="number"
            value={String(value ?? '')}
            onChange={(e) => {
              const n = Number(e.target.value)
              if (Number.isFinite(n)) setField(f.key, n)
            }}
            className="w-36"
          />
        )
      case 'text':
        return (
          <Input
            value={String(value ?? '')}
            onChange={(e) => setField(f.key, e.target.value)}
            className="w-72"
          />
        )
      case 'textarea':
        return (
          <Textarea
            value={String(value ?? '')}
            onChange={(e) => setField(f.key, e.target.value)}
            rows={3}
            className="w-full font-mono text-xs"
          />
        )
    }
  }

  return (
    <section className="flex flex-col gap-3">
      <SectionHeader title="Server Settings">
        <Button onClick={save} disabled={!dirty || busy}>
          {busy && <Spinner />} Save & apply
        </Button>
      </SectionHeader>
      <Card className="py-5">
        <CardContent className="flex flex-col gap-4 px-5">
          <p className="text-xs text-muted-foreground">
            Environment variables provide the defaults; edits below become overrides that apply
            immediately and persist across restarts ({'<data_dir>'}/settings.json). The master
            token, HOST/PORT, proxy trust and OIDC stay env-only.
          </p>
          {message && (
            <p
              className={cn(
                'rounded-2xl border px-3 py-2 text-sm',
                message.ok
                  ? 'border-emerald-500/30 bg-emerald-500/10 text-emerald-700 dark:text-emerald-400'
                  : 'border-red-500/30 bg-red-500/10 text-red-700 dark:text-red-400',
              )}
            >
              {message.text}
            </p>
          )}
          {FIELDS.map((f) => {
            const overridden = overrides[f.key] !== undefined && overrides[f.key] !== null
            return (
              <div key={f.key} className="flex flex-wrap items-center gap-3">
                <div className="flex w-64 shrink-0 flex-col">
                  <span className="text-sm">{f.label}</span>
                  {f.hint && <span className="text-xs text-muted-foreground">{f.hint}</span>}
                </div>
                <div className="min-w-40 flex-1">{control(f)}</div>
                {overridden ? (
                  <div className="flex items-center gap-2">
                    <TintBadge tint="amber">override</TintBadge>
                    <Tooltip>
                      <TooltipTrigger
                        render={
                          <Button
                            size="icon-xs"
                            variant="ghost"
                            onClick={() => resetField(f.key)}
                            aria-label={`Reset ${f.label} to default`}
                          />
                        }
                      >
                        <RotateCcwIcon />
                      </TooltipTrigger>
                      <TooltipContent>
                        Reset to env default ({JSON.stringify(data.defaults[f.key])})
                      </TooltipContent>
                    </Tooltip>
                  </div>
                ) : (
                  <span className="text-xs text-muted-foreground">default</span>
                )}
              </div>
            )
          })}
        </CardContent>
      </Card>
    </section>
  )
}
