import { RotateCcwIcon } from 'lucide-react'
import { useEffect, useState } from 'react'
import { SectionHeader } from './shared'
import { TintBadge } from './badges'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
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

interface GroupSpec {
  title: string
  description: string
  fields: FieldSpec[]
}

// Related settings live together; unrelated ones get their own card.
const GROUPS: GroupSpec[] = [
  {
    title: 'Gateway & Requests',
    description: 'Timeouts and size limits applied to every proxied request.',
    fields: [
      { key: 'gateway_timeout_secs', label: 'Gateway timeout (s)', kind: 'number', hint: 'Wait for a client to (re)connect before failing a request' },
      { key: 'gateway_response_timeout_secs', label: 'Response timeout (s)', kind: 'number', hint: 'Wait for a client to answer a dispatched request' },
      { key: 'max_body_size', label: 'Max request body (bytes)', kind: 'number', hint: 'Requests with larger bodies are rejected up front' },
    ],
  },
  {
    title: 'Capacity & Health',
    description: 'How many clients may connect and when one counts as down.',
    fields: [
      { key: 'max_tunnels', label: 'Max tunnel clients', kind: 'number', hint: 'Connection attempts beyond this are refused' },
      { key: 'client_down_threshold_secs', label: 'Client down threshold (s)', kind: 'number', hint: 'Missed-heartbeat window before a client leaves routing' },
    ],
  },
  {
    title: 'Routing & Failover',
    description: 'How requests pick a client and what happens when one is lost mid-request.',
    fields: [
      { key: 'lb_strategy', label: 'Load balancing', kind: 'select', options: ['round-robin', 'primary-standby', 'sticky'], hint: 'Strategy for picking a client from the routed pool' },
      { key: 'require_hostname_bind', label: 'Require hostname bind', kind: 'boolean', hint: 'Strict multi-tenant mode: unbound clients never receive traffic' },
      { key: 'failover_mode', label: 'In-flight failover', kind: 'select', options: ['fail', 'retry', 'wait', 'retry-wait'], hint: 'Reaction when the serving client drops mid-request' },
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
      { key: 'random_subdomain_suffix', label: 'Random subdomain pattern', kind: 'text', hint: 'e.g. example.com, *.example.com or *-test.example.com — * becomes a random label; empty = disabled' },
    ],
  },
  {
    title: 'Visitor Experience',
    description: 'What visitors see in front of and around the proxied services.',
    fields: [
      { key: 'auth_credentials', label: 'Visitor password', kind: 'text', hint: 'user:password gate in front of all proxied traffic; empty = disabled' },
      { key: 'custom_504_page', label: 'Custom 504 page (HTML)', kind: 'textarea', hint: 'Shown when no client answers in time' },
      { key: 'custom_503_page', label: 'Custom 503 maintenance page (HTML)', kind: 'textarea', hint: 'Shown for hostnames in maintenance mode' },
    ],
  },
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
            <SelectTrigger className="w-full">
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
          />
        )
      case 'text':
        return (
          <Input value={String(value ?? '')} onChange={(e) => setField(f.key, e.target.value)} />
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

  // Override marker + one-click reset to the env default, shown next to the
  // field label so the state of every setting is visible at a glance.
  const overrideControls = (f: FieldSpec) => {
    const overridden = overrides[f.key] !== undefined && overrides[f.key] !== null
    if (!overridden) return null
    return (
      <span className="inline-flex items-center gap-1">
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
      </span>
    )
  }

  const field = (f: FieldSpec) => {
    if (f.kind === 'boolean') {
      // Booleans read best as a bordered row with the switch on the right.
      return (
        <div key={f.key} className="flex items-center justify-between gap-3 rounded-3xl border px-4 py-3">
          <div className="flex flex-col gap-0.5">
            <span className="flex items-center gap-2 text-sm font-medium">
              {f.label} {overrideControls(f)}
            </span>
            {f.hint && <span className="text-xs text-muted-foreground">{f.hint}</span>}
          </div>
          {control(f)}
        </div>
      )
    }
    return (
      <div key={f.key} className={cn('flex flex-col gap-1.5', f.kind === 'textarea' && 'sm:col-span-2')}>
        <Label className="flex items-center gap-2">
          {f.label} {overrideControls(f)}
        </Label>
        {f.hint && <span className="text-xs text-muted-foreground">{f.hint}</span>}
        {control(f)}
      </div>
    )
  }

  return (
    <section className="flex flex-col gap-4">
      <SectionHeader
        title="Server Settings"
        description="Env vars provide the defaults; edits become live, persisted overrides. Master token, HOST/PORT, proxy trust and OIDC stay env-only."
      >
        {dirty && <span className="text-xs text-amber-600 dark:text-amber-400">Unsaved changes</span>}
        <Button onClick={save} disabled={!dirty || busy}>
          {busy && <Spinner />} Save & apply
        </Button>
      </SectionHeader>
      {message && (
        <p
          className={cn(
            'rounded-3xl border px-4 py-3 text-sm',
            message.ok
              ? 'border-emerald-500/30 bg-emerald-500/10 text-emerald-700 dark:text-emerald-400'
              : 'border-red-500/30 bg-red-500/10 text-red-700 dark:text-red-400',
          )}
        >
          {message.text}
        </p>
      )}
      <div className="grid grid-cols-1 gap-4 xl:grid-cols-2">
        {GROUPS.map((group) => (
          <Card key={group.title} className="gap-4 py-5">
            <CardHeader className="px-5">
              <CardTitle className="font-heading text-base">{group.title}</CardTitle>
              <CardDescription>{group.description}</CardDescription>
            </CardHeader>
            <CardContent className="grid grid-cols-1 gap-4 px-5 sm:grid-cols-2">
              {group.fields.map(field)}
            </CardContent>
          </Card>
        ))}
      </div>
    </section>
  )
}
