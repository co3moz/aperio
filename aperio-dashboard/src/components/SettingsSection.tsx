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
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table'
import {
  api,
  ApiError,
  type EnvironmentReport,
  type SettingsOverrides,
  type SettingsPayload,
} from '@/lib/api'
import { formatBytes, parseByteSize } from '@/lib/format'
import { cn } from '@/lib/utils'

type FieldKind = 'number' | 'bytes' | 'boolean' | 'select' | 'text' | 'textarea'

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
    title: 'Caching',
    description: 'Server-side response cache for services that opt in with cache: true.',
    fields: [
      { key: 'cache_enabled', label: 'Response cache', kind: 'boolean', hint: 'Cache-Control-driven GET cache; disabling clears stored entries' },
      { key: 'cache_max_bytes', label: 'Cache budget', kind: 'bytes', hint: 'Total memory for cached responses; entries closest to expiry are evicted first' },
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
      { key: 'auth_credentials', label: 'Visitor password', kind: 'text', hint: 'user:password gate in front of all proxied traffic; empty = disabled' },
      { key: 'custom_504_page', label: 'Custom 504 page (HTML)', kind: 'textarea', hint: 'Shown when no client answers in time' },
      { key: 'custom_503_page', label: 'Custom 503 maintenance page (HTML)', kind: 'textarea', hint: 'Shown for hostnames in maintenance mode' },
    ],
  },
]

/** Renders `bytes` as an editable human string when it maps cleanly to a
 *  unit ("10 MB"), otherwise as the raw number. */
function bytesToInput(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes < 0) return ''
  if (bytes === 0) return '0'
  const units = ['B', 'KB', 'MB', 'GB', 'TB']
  for (let i = units.length - 1; i >= 0; i--) {
    const size = 1024 ** i
    if (bytes % size === 0 && bytes >= size) return `${bytes / size} ${units[i]}`
  }
  return String(bytes)
}

/**
 * Byte-size input: accepts "10mb", "1.5 GB", "512K" or plain bytes, shows the
 * parsed size underneath, and only propagates valid values.
 */
function BytesInput({ value, onChange }: { value: number; onChange: (bytes: number) => void }) {
  const [text, setText] = useState(() => bytesToInput(value))
  const [lastValue, setLastValue] = useState(value)
  // Re-derive the text when the value changes from the outside (reset button,
  // reload) rather than from our own onChange.
  if (value !== lastValue) {
    setLastValue(value)
    if (parseByteSize(text) !== value) setText(bytesToInput(value))
  }
  const parsed = parseByteSize(text)
  const invalid = text.trim() !== '' && parsed === null
  return (
    <div className="flex flex-col gap-1">
      <Input
        value={text}
        placeholder="e.g. 10 MB, 1 GB, 65536"
        aria-invalid={invalid || undefined}
        onChange={(e) => {
          setText(e.target.value)
          const bytes = parseByteSize(e.target.value)
          if (bytes !== null) {
            setLastValue(bytes)
            onChange(bytes)
          }
        }}
      />
      <span className="text-xs text-muted-foreground">
        {invalid
          ? 'Not a size — use e.g. 10 MB, 1.5 GB, or plain bytes'
          : `= ${formatBytes(parsed ?? value)} (${(parsed ?? value).toLocaleString()} bytes)`}
      </span>
    </div>
  )
}

// What each env-only flag does, shown in the read-only reference table.
const ENV_FLAG_DESCRIPTIONS: Record<string, string> = {
  APERIO_TRUST_PROXY: 'Trust X-Forwarded-For / X-Real-IP from a fronting reverse proxy',
  APERIO_TRUSTED_PROXIES: 'Trusted proxy/CDN egress IPs or CIDRs used to resolve the real visitor IP',
  APERIO_TRUST_CF_HEADER: 'Cloudflare shorthand: trust the CF-Connecting-IP header',
  APERIO_REAL_IP_HEADER: 'Header consulted first for the visitor IP (behind CDN chains)',
  APERIO_SECURE_COOKIES: 'Session cookies carry the Secure flag (HTTPS only)',
  APERIO_IGNORE_CLIENT_AUTH: 'Ignore client-declared visitor passwords; the server keeps the gate',
  'APERIO_OIDC_*': 'OIDC single sign-on (issuer, client id/secret, redirect URL, scopes)',
  APERIO_METRICS: 'Prometheus metrics endpoint at /aperio/metrics',
  APERIO_METRICS_TOKEN: 'Token required to scrape the metrics endpoint',
  APERIO_ACCESS_LOG: 'JSONL access log file path (empty = disabled)',
}

/**
 * Read-only reference of env-only flags: their current values and, based on
 * whether the server runs in Docker, how to change them.
 */
function EnvReferenceCard({ environment }: { environment?: EnvironmentReport }) {
  if (!environment) return null
  const docker = environment.runtime === 'docker'
  return (
    <Card className="gap-4 py-5 xl:col-span-2">
      <CardHeader className="px-5">
        <CardTitle className="font-heading flex items-center gap-2 text-base">
          Environment Flags <TintBadge tint="gray">read-only</TintBadge>
        </CardTitle>
        <CardDescription>
          Security- and startup-critical flags stay environment-only so a compromised dashboard
          session cannot change them. The server is running{' '}
          {docker ? 'inside a container' : 'natively'} — to change one:
        </CardDescription>
      </CardHeader>
      <CardContent className="flex flex-col gap-4 px-5">
        <pre className="overflow-x-auto rounded-2xl border bg-muted/50 p-3 font-mono text-xs leading-relaxed">
          {docker
            ? `# docker run: add the flag and recreate the container
docker run -e APERIO_TRUST_PROXY=1 ... ghcr.io/co3moz/aperio-server

# docker compose: add it under environment: and run
#   docker compose up -d
services:
  aperio-server:
    environment:
      - APERIO_TRUST_PROXY=1`
            : `# shell: export before starting the server
export APERIO_TRUST_PROXY=1
aperio-server

# systemd: add to the unit and restart
[Service]
Environment=APERIO_TRUST_PROXY=1`}
        </pre>
        <Table>
          <TableHeader>
            <TableRow>
              <TableHead>Variable</TableHead>
              <TableHead>Current value</TableHead>
              <TableHead>Purpose</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {environment.flags.map((f) => (
              <TableRow key={f.key}>
                <TableCell>
                  <code className="font-mono text-xs">{f.key}</code>
                </TableCell>
                <TableCell>
                  <code className="break-all font-mono text-xs text-muted-foreground">
                    {f.value}
                  </code>
                </TableCell>
                <TableCell className="text-xs text-muted-foreground">
                  {ENV_FLAG_DESCRIPTIONS[f.key] ?? ''}
                </TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
      </CardContent>
    </Card>
  )
}

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
      case 'bytes':
        return <BytesInput value={Number(value ?? 0)} onChange={(bytes) => setField(f.key, bytes)} />

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
        <EnvReferenceCard environment={data.environment} />
      </div>
    </section>
  )
}
