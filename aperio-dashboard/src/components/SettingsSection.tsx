import { RotateCcwIcon } from 'lucide-react'
import { useEffect, useState } from 'react'
import { useUnsavedChanges } from '@/lib/unsaved'
import { usePaneFocus } from '@/lib/paneFocus'
import { SectionHeader } from './shared'
import {
  Accordion,
  AccordionContent,
  AccordionItem,
  AccordionTrigger,
} from '@/components/ui/accordion'
import { TintBadge } from './badges'
import { Button } from '@/components/ui/button'
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
import { useI18n } from '@/i18n'
import {
  GROUPS,
  settingAnchor,
  type FieldSpec,
} from '@/lib/settingsCatalog'


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
function BytesInput({
  value,
  onChange,
  min,
}: {
  value: number
  onChange: (bytes: number) => void
  min?: number
}) {
  const { t } = useI18n()
  const [text, setText] = useState(() => bytesToInput(value))
  const [lastValue, setLastValue] = useState(value)
  // Re-derive the text when the value changes from the outside (reset button,
  // reload) rather than from our own onChange.
  if (value !== lastValue) {
    setLastValue(value)
    if (parseByteSize(text) !== value) setText(bytesToInput(value))
  }
  const parsed = parseByteSize(text)
  // Below the server's floor is as invalid as unparseable: the three stream
  // watermarks reject 0, while `audit_max_size: 0` legitimately means "never
  // rotate", so the floor is per field rather than global.
  const tooSmall = min !== undefined && parsed !== null && parsed < min
  const invalid = text.trim() !== '' && (parsed === null || tooSmall)
  return (
    <div className="flex flex-col gap-1">
      <Input
        value={text}
        placeholder={t('e.g. 10 MB, 1 GB, 65536')}
        aria-invalid={invalid || undefined}
        onChange={(e) => {
          setText(e.target.value)
          const bytes = parseByteSize(e.target.value)
          if (bytes !== null && !(min !== undefined && bytes < min)) {
            setLastValue(bytes)
            onChange(bytes)
          }
        }}
      />
      <span className="text-xs text-muted-foreground">
        {tooSmall
          ? t('Must be at least {min}', { min: formatBytes(min ?? 0) })
          : invalid
            ? t('Not a size, use e.g. 10 MB, 1.5 GB, or plain bytes')
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
function EnvReference({ environment }: { environment?: EnvironmentReport }) {
  const { t } = useI18n()
  if (!environment) return null
  const docker = environment.runtime === 'docker'
  return (
    <div className="flex flex-col gap-4">
      <p className="text-xs text-muted-foreground">
          {docker
            ? t('Security- and startup-critical flags stay environment-only so a compromised dashboard session cannot change them. The server is running inside a container, to change one:')
            : t('Security- and startup-critical flags stay environment-only so a compromised dashboard session cannot change them. The server is running natively, to change one:')}
      </p>
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
              <TableHead>{t('Variable')}</TableHead>
              <TableHead>{t('Current value')}</TableHead>
              <TableHead>{t('Purpose')}</TableHead>
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
                  {t(ENV_FLAG_DESCRIPTIONS[f.key] ?? '')}
                </TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
    </div>
  )
}

/**
 * Dashboard-editable server settings. Environment variables provide the
 * defaults; edits become overrides that apply live and persist in
 * `<data_dir>/settings.json`. The master token, HOST/PORT, proxy trust and
 * OIDC remain env-only.
 */
export function SettingsSection() {
  const { t } = useI18n()
  const [data, setData] = useState<SettingsPayload | null>(null)
  const [overrides, setOverrides] = useState<SettingsOverrides>({})
  const [dirty, setDirty] = useState(false)
  const [busy, setBusy] = useState(false)
  const [message, setMessage] = useState<{ ok: boolean; text: string } | null>(null)
  // Nothing here is applied until Save, and the dialog around this form can be
  // dismissed (or the page reloaded) in one keystroke.
  useUnsavedChanges(dirty)

  // Which groups are expanded. Controlled rather than left to the accordion,
  // because arriving from a search means opening the one group that holds the
  // setting somebody asked for.
  const [openGroups, setOpenGroups] = useState<string[]>([])
  const focus = usePaneFocus()
  useEffect(() => {
    const key = focus?.target.startsWith('setting:') ? focus.target.slice('setting:'.length) : null
    if (!key) return
    const index = GROUPS.findIndex((g) => g.fields.some((f) => f.key === key))
    if (index < 0) return
    setOpenGroups((open) => (open.includes(String(index)) ? open : [...open, String(index)]))
    // After the panel has been laid out, not before: a collapsed group has no
    // position to scroll to. Marked rather than only scrolled, since landing
    // in the middle of sixty inputs says nothing about which one was meant.
    const timer = setTimeout(() => {
      const el = document.getElementById(settingAnchor(key))
      if (!el) return
      el.scrollIntoView({ block: 'center', behavior: 'smooth' })
      el.dataset.found = 'true'
      setTimeout(() => delete el.dataset.found, 2000)
    }, 150)
    return () => clearTimeout(timer)
  }, [focus])

  const load = () => {
    api
      .settings()
      .then((payload) => {
        setData(payload)
        setOverrides({ ...payload.overrides })
        setDirty(false)
      })
      .catch((e) => {
        // `if (!data) return null` below means a failed load renders an empty
        // pane. Saying so is the difference between "there is nothing here"
        // and "this did not load".
        setMessage({ ok: false, text: e instanceof ApiError ? e.message : String(e) })
      })
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

  // Ctrl+S / Cmd+S saves pending changes instead of the browser's save-page
  // dialog, matching editor muscle memory on a settings form.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === 's') {
        e.preventDefault()
        if (dirty && !busy) void save()
      }
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [dirty, busy, overrides])

  const save = async () => {
    setBusy(true)
    setMessage(null)
    try {
      await api.updateSettings(overrides)
      setMessage({ ok: true, text: t('Settings applied and persisted.') })
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
      case 'select': {
        const chosen = String(value ?? '')
        const explain = f.optionHints?.[chosen]
        return (
          <div className="flex flex-col gap-1.5">
            <Select value={chosen} onValueChange={(v) => setField(f.key, v as string)}>
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
            {/* What the chosen option does, under the picker. The option names
                are identifiers, not sentences: `sticky` says nothing about what
                happens to a request, which is the thing being decided. */}
            {explain && (
              <p className="rounded-2xl border border-primary/20 bg-primary/5 px-3 py-2 text-xs text-muted-foreground">
                {t(explain)}
              </p>
            )}
          </div>
        )
      }
      case 'number':
        return (
          <Input
            type="number"
            value={String(value ?? '')}
            onChange={(e) => {
              // Clearing the box means "no override", not zero. `Number('')`
              // is 0, so an emptied field used to save a real 0 for the
              // setting instead of falling back to the server's default.
              const raw = e.target.value.trim()
              if (raw === '') {
                resetField(f.key)
                return
              }
              const n = Number(raw)
              if (Number.isFinite(n)) setField(f.key, n)
            }}
          />
        )
      case 'bytes':
        return (
          <BytesInput
            value={Number(value ?? 0)}
            min={f.min}
            onChange={(bytes) => setField(f.key, bytes)}
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

  /** True when this setting carries a stored override rather than the value
   *  the server started with. */
  const isOverridden = (key: string) => overrides[key] !== undefined && overrides[key] !== null

  /** True when aperio-server.yaml sets this key. The file wins, so the field
   *  is shown rather than offered: typing here would be refused on save. */
  const fromFile = (key: string) => (data?.file_keys ?? []).includes(key)

  // Override marker + one-click reset to the env default, shown next to the
  // field label so the state of every setting is visible at a glance.
  const overrideControls = (f: FieldSpec) => {
    const overridden = isOverridden(f.key)
    if (!overridden) return null
    return (
      <span className="inline-flex items-center gap-1">
        <TintBadge tint="amber">{t('override')}</TintBadge>
        <Tooltip>
          <TooltipTrigger
            render={
              <Button
                size="icon-xs"
                variant="ghost"
                onClick={() => resetField(f.key)}
                aria-label={t('Reset {label} to default', { label: t(f.label) })}
              />
            }
          >
            <RotateCcwIcon />
          </TooltipTrigger>
          <TooltipContent>
            {t('Reset to env default ({value})', { value: JSON.stringify(data.defaults[f.key]) })}
          </TooltipContent>
        </Tooltip>
      </span>
    )
  }

  /** Marker for a setting aperio-server.yaml owns. */
  const fileMarker = (f: FieldSpec) =>
    fromFile(f.key) ? (
      <Tooltip>
        <TooltipTrigger
          render={
            <span>
              <TintBadge tint="blue">{t('from file')}</TintBadge>
            </span>
          }
        />
        <TooltipContent>
          {t(
            'aperio-server.yaml sets this, and the file wins. Edit it there: a change made here would be refused, because a stored override the file contradicts is exactly the invisible state this avoids.',
          )}
        </TooltipContent>
      </Tooltip>
    ) : null

  const field = (f: FieldSpec) => {
    if (f.kind === 'boolean') {
      // Booleans read best as a bordered row with the switch on the right.
      return (
        <div
          key={f.key}
          id={settingAnchor(f.key)}
          className="flex items-center justify-between gap-3 rounded-3xl border px-4 py-3 outline-2 outline-offset-4 outline-transparent transition-[outline-color] data-[found=true]:outline-primary"
        >
          <div className="flex flex-col gap-0.5">
            <span className="flex items-center gap-2 text-sm font-medium">
              {t(f.label)} {overrideControls(f)} {fileMarker(f)}
            </span>
            {f.hint && <span className="text-xs text-muted-foreground">{t(f.hint)}</span>}
          </div>
          {control(f)}
        </div>
      )
    }
    return (
      <div
        key={f.key}
        id={settingAnchor(f.key)}
        className={cn(
          'flex flex-col gap-1.5 rounded-3xl transition-[outline-color] outline-2 outline-offset-8 outline-transparent data-[found=true]:outline-primary',
          f.kind === 'textarea' && '@2xl:col-span-2',
        )}
      >
        <Label className="flex items-center gap-2">
          {t(f.label)} {overrideControls(f)} {fileMarker(f)}
        </Label>
        {f.hint && <span className="text-xs text-muted-foreground">{t(f.hint)}</span>}
        {control(f)}
      </div>
    )
  }

  return (
    <section className="flex flex-col gap-4">
      <SectionHeader
        title={t('Server Settings')}
        description={t('Env vars provide the defaults; edits become live, persisted overrides. Master token, HOST/PORT, proxy trust and OIDC stay env-only.')}
      >
        {dirty && (
          <span className="text-xs text-amber-600 dark:text-amber-400">{t('Unsaved changes')}</span>
        )}
        <Button onClick={save} disabled={!dirty || busy} title={t('Save & apply (Ctrl+S)')}>
          {busy && <Spinner />} {t('Save & apply')}
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
      {/* One column of collapsed groups rather than two columns of open
          cards: side by side, two unrelated groups of switches read as one
          undifferentiated wall, and finding a setting meant scanning both
          columns. Collapsed, the headings are the index. */}
      <Accordion
        className="w-full"
        value={openGroups}
        onValueChange={(v) => setOpenGroups(v as string[])}
      >
        {GROUPS.map((group, i) => {
          // How many of this group's settings are not what the server started
          // with. On the header rather than inside, because the reason to
          // want this number is to find the group you did not think to open:
          // a setting changed once from the dashboard is invisible until
          // something it does surprises you.
          const changed = group.fields.filter((f) => isOverridden(f.key)).length
          return (
          <AccordionItem key={group.title} value={String(i)}>
            <AccordionTrigger>
              <span className="flex-1 text-left">{t(group.title)}</span>
              {changed > 0 && (
                <Tooltip>
                  <TooltipTrigger
                    render={
                      <span>
                        <TintBadge tint="amber">{changed}</TintBadge>
                      </span>
                    }
                  />
                  <TooltipContent>
                    {t('{count} setting(s) here differ from what the server started with', {
                      count: changed,
                    })}
                  </TooltipContent>
                </Tooltip>
              )}
            </AccordionTrigger>
            <AccordionContent>
              <p className="mb-4 text-xs text-muted-foreground">{t(group.description)}</p>
              {/* Container query, not `sm:`: these live in a dialog whose
                  width does not follow the viewport's, so asking the screen
                  how much room there is gave two columns in a 620px pane and
                  wrapped every label onto three lines. */}
              <div className="@container">
                <div className="grid grid-cols-1 gap-4 @2xl:grid-cols-2">
                  {group.fields.map(field)}
                </div>
              </div>
            </AccordionContent>
          </AccordionItem>
          )
        })}
        {data.environment && (
          <AccordionItem value="environment">
            <AccordionTrigger>
              <span className="flex flex-1 items-center gap-2 text-left">
                {t('Environment Flags')} <TintBadge tint="gray">{t('read-only')}</TintBadge>
              </span>
            </AccordionTrigger>
            <AccordionContent>
              <EnvReference environment={data.environment} />
            </AccordionContent>
          </AccordionItem>
        )}
      </Accordion>
    </section>
  )
}

/**
 * Dump export/import: download a logical JSON dump of tokens, webhooks,
 * users and settings overrides, or apply one (replacing the stores).
 */
