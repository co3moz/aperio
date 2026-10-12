import { RotateCcwIcon } from 'lucide-react'
import { useEffect, useMemo, useState } from 'react'
import YAML from 'yaml'
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
import { api, ApiError, pathIn, type ServerSettings, type SettingsPayload } from '@/lib/api'
import { getAt, setAt, type JsonSchema } from '@/lib/configSchema'
import { SERVER_GROUPS } from '@/lib/configGroups'
import { cn } from '@/lib/utils'
import { formatRelativeTime } from '@/lib/format'
import { useI18n } from '@/i18n'
import { catalogEntry, settingAnchor } from '@/lib/settingsCatalog'

/** What a stored credential reads as; writing it back keeps the credential. */
const MASK = '********'

/** How a setting's value is entered. */
type Editor =
  | 'boolean'
  /** An `Option<bool>`: on, off, or the server's own choice. */
  | 'maybe'
  | 'select'
  | 'number'
  | 'duration'
  | 'size'
  | 'text'
  /** A list of plain values, one per line. */
  | 'lines'
  /** Anything with structure of its own: a rule list, a gate, a map. */
  | 'yaml'
  /** A block of settings of its own (`limits.ip_rate`). */
  | 'group'

interface Setting {
  path: string
  key: string
  editor: Editor
  description?: string
  options?: string[]
  example?: string
  /** A `lines` list of numbers rather than of words. */
  numeric?: boolean
  children?: Setting[]
}

/** The forms a duration and a size are written in, as the server parses them. */
const DURATION = /^\s*[0-9]+\s*(ms|s|sec|secs|m|min|h|d|w)?\s*$/i
const SIZE = /^\s*[0-9]+\s*([kmg](i?b)?|b)?\s*$/i

/** Settings whose value is structured even where the schema has a shape for
 *  it: a gate is a credential, a method block or a list of them, and the
 *  header rules are maps. Easier to read and to write as yaml. */
const YAML_PATHS = new Set(['access.gate', 'headers'])

function deref(node: JsonSchema, root: JsonSchema): JsonSchema {
  if (!node.$ref) return node
  return root.$defs?.[node.$ref.split('/').pop() ?? ''] ?? node
}

/** The branches of a union, flattened, without `null`. */
function branches(node: JsonSchema, root: JsonSchema): JsonSchema[] {
  const resolved = deref(node, root)
  const list = resolved.anyOf ?? resolved.oneOf
  if (!list) return resolved.type === 'null' ? [] : [resolved]
  return list.flatMap((b) => branches(b, root))
}

function types(node: JsonSchema): string[] {
  return Array.isArray(node.type) ? node.type : node.type ? [node.type] : []
}

/** Reads one property of the settings schema into how it is edited. */
function settingOf(key: string, path: string, raw: JsonSchema, root: JsonSchema): Setting {
  const outer = deref(raw, root)
  const description = raw.description ?? outer.description
  const example = (raw.examples ?? outer.examples)?.map((e) =>
    typeof e === 'string' ? e : JSON.stringify(e),
  )[0]
  const base = { key, path, description, example }
  if (YAML_PATHS.has(path)) return { ...base, editor: 'yaml' }
  const all = branches(raw, root)
  const patterns = all.map((b) => b.pattern ?? '')
  if (patterns.some((p) => p.includes('ms|s'))) return { ...base, editor: 'duration' }
  if (patterns.some((p) => p.includes('[kKmMgG]'))) return { ...base, editor: 'size' }
  if (patterns.some((p) => p.length > 0) && all.some((b) => b.const === 'auto' || b.enum)) {
    return { ...base, editor: 'duration' }
  }
  const nullable =
    types(outer).includes('null') ||
    (outer.anyOf ?? outer.oneOf ?? []).some((b) => deref(b, root).type === 'null')
  const enumBranch = all.find((b) => b.enum?.length)
  if (enumBranch) return { ...base, editor: 'select', options: enumBranch.enum!.map(String) }
  const kinds = all.flatMap(types)
  if (kinds.includes('boolean')) return { ...base, editor: nullable ? 'maybe' : 'boolean' }
  if (kinds.includes('integer') || kinds.includes('number')) return { ...base, editor: 'number' }
  if (kinds.includes('array')) {
    const item = branches(all.find((b) => types(b).includes('array'))?.items ?? {}, root)
    const scalar = item.every(
      (b) =>
        (types(b).includes('string') || types(b).includes('integer')) && !b.properties,
    )
    const numeric = item.every((b) => types(b).every((k) => k === 'integer' || k === 'number'))
    return { ...base, editor: scalar ? 'lines' : 'yaml', numeric }
  }
  const object = all.find((b) => b.properties)
  if (object && all.length === 1) {
    return {
      ...base,
      editor: 'group',
      children: Object.entries(object.properties!).map(([k, v]) =>
        settingOf(k, `${path}.${k}`, v, root),
      ),
    }
  }
  if (kinds.includes('string') && all.length === 1) return { ...base, editor: 'text' }
  return { ...base, editor: 'yaml' }
}

/** Every section of the schema, as settings: a section's fields, or the
 *  section itself where it is a list. */
function sectionsOf(schema: JsonSchema): Map<string, Setting[]> {
  const out = new Map<string, Setting[]>()
  for (const [key, raw] of Object.entries(schema.properties ?? {})) {
    const resolved = deref(raw, schema)
    if (resolved.properties && !YAML_PATHS.has(key)) {
      out.set(
        key,
        Object.entries(resolved.properties).map(([field, node]) =>
          settingOf(field, `${key}.${field}`, node, schema),
        ),
      )
    } else {
      out.set(key, [settingOf(key, key, raw, schema)])
    }
  }
  return out
}

/** Every leaf path under `setting`, what a search and a change marker read. */
function leaves(setting: Setting): string[] {
  return setting.editor === 'group'
    ? (setting.children ?? []).flatMap(leaves)
    : [setting.path]
}

/** `snake_case` as words, for a setting the catalogue has no label for. */
function humanize(key: string): string {
  const words = key.replaceAll('_', ' ')
  return words.charAt(0).toUpperCase() + words.slice(1)
}

/** A yaml rendering of a structured value, empty for an empty one. */
function asYaml(value: unknown): string {
  if (value === undefined || value === null) return ''
  if (Array.isArray(value) && value.length === 0) return ''
  if (typeof value === 'object' && Object.keys(value as object).length === 0) return ''
  return YAML.stringify(value).trimEnd()
}

/**
 * A structured value edited as yaml. The text is kept as typed, and the
 * document only takes it when it parses, so a half-typed list does not wipe
 * the setting it is replacing.
 */
function YamlInput({
  value,
  onChange,
  onValidity,
}: {
  value: unknown
  onChange: (value: unknown) => void
  onValidity: (error: string | null) => void
}) {
  const { t } = useI18n()
  const [text, setText] = useState(() => asYaml(value))
  const [error, setError] = useState<string | null>(null)
  return (
    <div className="flex flex-col gap-1">
      <Textarea
        value={text}
        rows={Math.min(14, Math.max(3, text.split('\n').length + 1))}
        className="w-full font-mono text-xs"
        aria-invalid={error ? true : undefined}
        onChange={(e) => {
          const next = e.target.value
          setText(next)
          try {
            const parsed: unknown = next.trim() === '' ? null : YAML.parse(next)
            setError(null)
            onValidity(null)
            onChange(parsed)
          } catch (err) {
            const message = err instanceof Error ? err.message.split('\n')[0] : String(err)
            setError(message)
            onValidity(message)
          }
        }}
      />
      {error && (
        <span className="text-xs text-red-700 dark:text-red-400">
          {t('Not valid YAML: {error}', { error })}
        </span>
      )}
    </div>
  )
}

/**
 * The server's settings, as one document edited in place.
 *
 * The form is built from the settings schema the server serves, so a setting
 * added to the server appears here without a dashboard change; the catalogue
 * adds translated labels, hints and option explanations to the common ones.
 * Save sends the whole document. A credential reads as the mask and is sent
 * back as the mask, which the server reads as "keep the stored one".
 */
export function SettingsSection() {
  const { t } = useI18n()
  const [payload, setPayload] = useState<SettingsPayload | null>(null)
  const [schema, setSchema] = useState<JsonSchema | null>(null)
  const [doc, setDoc] = useState<ServerSettings>({})
  const [dirty, setDirty] = useState(false)
  const [busy, setBusy] = useState(false)
  const [invalid, setInvalid] = useState<Record<string, string>>({})
  const [message, setMessage] = useState<{ ok: boolean; text: string } | null>(null)
  // A remount of the yaml editors after a load, so they show the stored text
  // rather than what was typed before the save.
  const [generation, setGeneration] = useState(0)
  // Nothing here is applied until Save, and the dialog around this form can be
  // dismissed (or the page reloaded) in one keystroke.
  useUnsavedChanges(dirty)

  const load = () => {
    Promise.all([api.settings(), api.configSchema('server')])
      .then(([settings, served]) => {
        setPayload(settings)
        setSchema(served)
        setDoc(settings.settings)
        setDirty(false)
        setInvalid({})
        setGeneration((g) => g + 1)
      })
      .catch((e) => {
        setMessage({ ok: false, text: e instanceof ApiError ? e.message : String(e) })
      })
  }
  useEffect(load, [])

  const sections = useMemo(() => (schema ? sectionsOf(schema) : new Map<string, Setting[]>()), [schema])

  // Which groups are expanded. Controlled rather than left to the accordion,
  // because arriving from a search means opening the one group that holds the
  // setting somebody asked for.
  const [openGroups, setOpenGroups] = useState<string[]>([])
  const focus = usePaneFocus()
  useEffect(() => {
    const path = focus?.target.startsWith('setting:') ? focus.target.slice('setting:'.length) : null
    if (!path || sections.size === 0) return
    const section = path.split('.')[0]
    const index = SERVER_GROUPS.findIndex((g) => g.keys.includes(section))
    if (index < 0) return
    setOpenGroups((open) => (open.includes(String(index)) ? open : [...open, String(index)]))
    // After the panel has been laid out, not before: a collapsed group has no
    // position to scroll to. Marked rather than only scrolled, since landing
    // in the middle of a hundred inputs says nothing about which one was meant.
    const timer = setTimeout(() => {
      const el = document.getElementById(settingAnchor(path))
      if (!el) return
      el.scrollIntoView({ block: 'center', behavior: 'smooth' })
      el.dataset.found = 'true'
      setTimeout(() => delete el.dataset.found, 2000)
    }, 150)
    return () => clearTimeout(timer)
  }, [focus, sections])

  const setField = (path: string, value: unknown) => {
    setDoc((d) => setAt(d, path, value))
    setDirty(true)
    setMessage(null)
  }
  const markValidity = (path: string, error: string | null) => {
    setInvalid((v) => {
      const next = { ...v }
      if (error) next[path] = error
      else delete next[path]
      return next
    })
  }

  // Ctrl+S / Cmd+S saves pending changes instead of the browser's save-page
  // dialog, matching editor muscle memory on a settings form.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === 's') {
        e.preventDefault()
        if (dirty && !busy && Object.keys(invalid).length === 0) void save()
      }
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [dirty, busy, doc, invalid])

  const save = async () => {
    setBusy(true)
    setMessage(null)
    try {
      await api.updateSettings(doc)
      setMessage({ ok: true, text: t('Settings applied and persisted.') })
      load()
    } catch (e) {
      // The server names the setting it refused, or says that a
      // security-relevant change needs a fresh sign-in; either way the
      // sentence is the useful part.
      setMessage({ ok: false, text: e instanceof ApiError ? e.message : String(e) })
    } finally {
      setBusy(false)
    }
  }

  // Who changed a setting, and when: the audit log knows, the settings
  // document does not. Hooks run even while the document is loading so their
  // order stays the same on every render.
  const [lastChange, setLastChange] = useState<Record<string, { actor: string; timestamp: string }>>({})
  useEffect(() => {
    if (!payload) return
    let live = true
    api
      .audit({ event: 'settings_updated', limit: 100 })
      .then((rows) => {
        if (!live) return
        const seen: Record<string, { actor: string; timestamp: string }> = {}
        for (const row of rows) {
          for (const path of row.details.split(',').map((k) => k.trim())) {
            if (path && !seen[path]) seen[path] = { actor: row.actor, timestamp: row.timestamp }
          }
        }
        setLastChange(seen)
      })
      .catch(() => {})
    return () => {
      live = false
    }
  }, [payload])

  if (!payload || !schema) {
    return message ? (
      <p className="rounded-3xl border border-red-500/30 bg-red-500/10 px-4 py-3 text-sm text-red-700 dark:text-red-400">
        {message.text}
      </p>
    ) : null
  }

  const security = (path: string) => pathIn(path, payload.security_paths)
  const restart = (path: string) => pathIn(path, payload.restart_paths)
  const secret = (path: string) => pathIn(path, payload.secret_paths)
  const pending = (path: string) =>
    payload.restart_pending.some((p) => p === path || p.startsWith(`${path}.`))
  const changedFromStored = (path: string) =>
    JSON.stringify(getAt(doc, path) ?? null) !== JSON.stringify(getAt(payload.settings, path) ?? null)

  const markers = (s: Setting) => {
    const change = lastChange[s.path]
    return (
      <span className="inline-flex flex-wrap items-center gap-1">
        {security(s.path) && (
          <Tooltip>
            <TooltipTrigger
              render={
                <span>
                  <TintBadge tint="red">{t('security')}</TintBadge>
                </span>
              }
            />
            <TooltipContent>
              {t('Changing this takes Admin in every organization and a sign-in from the last ten minutes.')}
            </TooltipContent>
          </Tooltip>
        )}
        {restart(s.path) && (
          <Tooltip>
            <TooltipTrigger
              render={
                <span>
                  <TintBadge tint={pending(s.path) ? 'amber' : 'gray'}>
                    {pending(s.path) ? t('restart pending') : t('restart')}
                  </TintBadge>
                </span>
              }
            />
            <TooltipContent>
              {t('Read when the server starts: a change is stored and takes effect at the next start.')}
            </TooltipContent>
          </Tooltip>
        )}
        {change && (
          <span className="text-xs text-muted-foreground" title={change.timestamp}>
            {t('by {actor}, {when}', { actor: change.actor, when: formatRelativeTime(change.timestamp, t) })}
          </span>
        )}
        {changedFromStored(s.path) && (
          <Tooltip>
            <TooltipTrigger
              render={
                <Button
                  size="icon-xs"
                  variant="ghost"
                  onClick={() => setField(s.path, getAt(payload.settings, s.path))}
                  aria-label={t('Undo')}
                />
              }
            >
              <RotateCcwIcon />
            </TooltipTrigger>
            <TooltipContent>{t('Undo')}</TooltipContent>
          </Tooltip>
        )}
      </span>
    )
  }

  const control = (s: Setting) => {
    const value = getAt(doc, s.path)
    switch (s.editor) {
      case 'boolean':
        return <Switch checked={Boolean(value)} onCheckedChange={(v) => setField(s.path, v)} />
      case 'maybe': {
        const chosen = value === true ? 'on' : value === false ? 'off' : 'auto'
        return (
          <Select
            value={chosen}
            onValueChange={(v) => setField(s.path, v === 'on' ? true : v === 'off' ? false : null)}
          >
            <SelectTrigger className="w-full">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="auto">{t('automatic')}</SelectItem>
              <SelectItem value="on">{t('on')}</SelectItem>
              <SelectItem value="off">{t('off')}</SelectItem>
            </SelectContent>
          </Select>
        )
      }
      case 'select': {
        const chosen = String(value ?? '')
        const explain = catalogEntry(s.path)?.optionHints?.[chosen]
        return (
          <div className="flex flex-col gap-1.5">
            <Select value={chosen} onValueChange={(v) => setField(s.path, v as string)}>
              <SelectTrigger className="w-full">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {(s.options ?? []).map((o) => (
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
            value={value === undefined || value === null ? '' : String(value)}
            placeholder={s.example}
            onChange={(e) => {
              // Clearing the box means "the default", not zero.
              const raw = e.target.value.trim()
              if (raw === '') {
                setField(s.path, null)
                return
              }
              const n = Number(raw)
              if (Number.isFinite(n)) setField(s.path, n)
            }}
          />
        )
      case 'duration':
      case 'size': {
        const text = value === undefined || value === null ? '' : String(value)
        const pattern = s.editor === 'duration' ? DURATION : SIZE
        const bad = text.trim() !== '' && text !== 'auto' && !pattern.test(text)
        return (
          <div className="flex flex-col gap-1">
            <Input
              value={text}
              placeholder={s.example}
              aria-invalid={bad || undefined}
              onChange={(e) => {
                const next = e.target.value
                setField(s.path, next.trim() === '' ? null : next.trim())
                const wrong = next.trim() !== '' && next.trim() !== 'auto' && !pattern.test(next)
                markValidity(
                  s.path,
                  wrong
                    ? s.editor === 'duration'
                      ? t('Not a duration, use e.g. 30s, 5m, 2h, 1d')
                      : t('Not a size, use e.g. 512KB, 10MB, 1GB')
                    : null,
                )
              }}
            />
            {bad && (
              <span className="text-xs text-red-700 dark:text-red-400">{invalid[s.path]}</span>
            )}
          </div>
        )
      }
      case 'text': {
        if (secret(s.path)) {
          const stored = value === MASK
          return (
            <div className="flex items-center gap-2">
              <Input
                type="password"
                autoComplete="new-password"
                value={stored ? '' : String(value ?? '')}
                placeholder={stored ? t('stored, type to replace') : s.example}
                onChange={(e) => setField(s.path, e.target.value === '' ? (stored ? MASK : null) : e.target.value)}
              />
              {value !== undefined && value !== null && (
                <Button variant="outline" size="sm" onClick={() => setField(s.path, null)}>
                  {t('Clear')}
                </Button>
              )}
            </div>
          )
        }
        const long = typeof value === 'string' && (value.length > 80 || value.includes('\n'))
        return long || s.path.startsWith('pages.') ? (
          <Textarea
            value={String(value ?? '')}
            rows={4}
            className="w-full font-mono text-xs"
            onChange={(e) => setField(s.path, e.target.value === '' ? null : e.target.value)}
          />
        ) : (
          <Input
            value={String(value ?? '')}
            placeholder={s.example}
            onChange={(e) => setField(s.path, e.target.value === '' ? null : e.target.value)}
          />
        )
      }
      case 'lines':
        return (
          <Textarea
            value={Array.isArray(value) ? value.map(String).join('\n') : ''}
            rows={Math.max(2, Array.isArray(value) ? value.length + 1 : 2)}
            placeholder={t('One per line')}
            className="w-full font-mono text-xs"
            onChange={(e) =>
              setField(
                s.path,
                e.target.value
                  .split('\n')
                  .map((l) => l.trim())
                  .filter(Boolean)
                  .map((l) => (s.numeric && /^-?\d+$/.test(l) ? Number(l) : l)),
              )
            }
          />
        )
      case 'yaml':
        return (
          <YamlInput
            key={`${s.path}-${generation}`}
            value={value}
            onChange={(v) => setField(s.path, v)}
            onValidity={(error) => markValidity(s.path, error)}
          />
        )
      case 'group':
        return null
    }
  }

  const field = (s: Setting): React.ReactNode => {
    const entry = catalogEntry(s.path)
    const label = entry ? t(entry.label) : humanize(s.key)
    const hint = entry?.hint ? t(entry.hint) : s.description
    if (s.editor === 'group') {
      return (
        <div key={s.path} className="flex flex-col gap-3 rounded-3xl border px-4 py-3 @2xl:col-span-2">
          <span className="flex items-center gap-2 text-sm font-medium">
            {label} <code className="text-xs text-muted-foreground">{s.path}</code> {markers(s)}
          </span>
          {hint && <span className="text-xs text-muted-foreground">{hint}</span>}
          <div className="grid grid-cols-1 gap-4 @2xl:grid-cols-2">{(s.children ?? []).map(field)}</div>
        </div>
      )
    }
    if (s.editor === 'boolean') {
      // Booleans read best as a bordered row with the switch on the right.
      return (
        <div
          key={s.path}
          id={settingAnchor(s.path)}
          className="flex items-center justify-between gap-3 rounded-3xl border px-4 py-3 outline-2 outline-offset-4 outline-transparent transition-[outline-color] data-[found=true]:outline-primary"
        >
          <div className="flex flex-col gap-0.5">
            <span className="flex flex-wrap items-center gap-2 text-sm font-medium">
              {label} <code className="text-xs font-normal text-muted-foreground">{s.path}</code> {markers(s)}
            </span>
            {hint && <span className="text-xs text-muted-foreground">{hint}</span>}
          </div>
          {control(s)}
        </div>
      )
    }
    return (
      <div
        key={s.path}
        id={settingAnchor(s.path)}
        className={cn(
          'flex flex-col gap-1.5 rounded-3xl transition-[outline-color] outline-2 outline-offset-8 outline-transparent data-[found=true]:outline-primary',
          (s.editor === 'yaml' || s.editor === 'lines' || s.path.startsWith('pages.')) &&
            '@2xl:col-span-2',
        )}
      >
        <Label className="flex flex-wrap items-center gap-2">
          {label} <code className="text-xs font-normal text-muted-foreground">{s.path}</code> {markers(s)}
        </Label>
        {hint && <span className="text-xs text-muted-foreground">{hint}</span>}
        {control(s)}
      </div>
    )
  }

  const blocked = Object.keys(invalid).length > 0
  return (
    <section className="flex flex-col gap-4">
      <SectionHeader
        title={t('Server Settings')}
        description={t(
          'Every setting lives in the server’s database and applies when saved. Security-relevant ones take a sign-in from the last ten minutes; a few are read at startup and wait for a restart.',
        )}
      >
        {dirty && (
          <span className="text-xs text-amber-600 dark:text-amber-400">{t('Unsaved changes')}</span>
        )}
        <Button onClick={save} disabled={!dirty || busy || blocked} title={t('Save & apply (Ctrl+S)')}>
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
      {payload.restart_pending.length > 0 && (
        <p className="rounded-3xl border border-amber-500/30 bg-amber-500/10 px-4 py-3 text-sm text-amber-800 dark:text-amber-300">
          {t('Saved, and waiting for a restart: {paths}', {
            paths: payload.restart_pending.join(', '),
          })}
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
        {SERVER_GROUPS.map((group, i) => {
          const settings = group.keys.flatMap((key) => sections.get(key) ?? [])
          // How many of this group's settings are edited and not yet saved.
          // On the header rather than inside, because the reason to want the
          // number is to find the group you did not think to open.
          const edited = settings.flatMap(leaves).filter(changedFromStored).length
          return (
            <AccordionItem key={group.title} value={String(i)}>
              <AccordionTrigger>
                <span className="flex-1 text-left">{t(group.title)}</span>
                {edited > 0 && <TintBadge tint="amber">{edited}</TintBadge>}
              </AccordionTrigger>
              <AccordionContent>
                <p className="mb-4 text-xs text-muted-foreground">{t(group.description)}</p>
                {/* Container query, not `sm:`: these live in a dialog whose
                    width does not follow the viewport's, so asking the screen
                    how much room there is gave two columns in a 620px pane. */}
                <div className="@container flex flex-col gap-6">
                  {group.keys.map((key) => {
                    const list = sections.get(key)
                    if (!list) return null
                    const own = schema.properties?.[key]
                    return (
                      <div key={key} className="flex flex-col gap-3">
                        <div className="flex flex-col gap-0.5">
                          <code className="text-xs font-medium">{key}</code>
                          {own?.description && list.length > 1 && (
                            <span className="text-xs text-muted-foreground">{own.description}</span>
                          )}
                        </div>
                        <div className="grid grid-cols-1 gap-4 @2xl:grid-cols-2">{list.map(field)}</div>
                      </div>
                    )
                  })}
                </div>
              </AccordionContent>
            </AccordionItem>
          )
        })}
      </Accordion>
    </section>
  )
}
