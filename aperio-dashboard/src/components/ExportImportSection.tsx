import { useState } from 'react'
import { SectionHeader } from './shared'
import { Button } from '@/components/ui/button'
import { Checkbox } from '@/components/ui/checkbox'
import { Spinner } from '@/components/ui/spinner'
import { useI18n } from '@/i18n'
import { cn } from '@/lib/utils'

/**
 * One section of a dump. `key` is the name the API takes in `?include=` and
 * writes as the JSON key, so there is a single spelling to know.
 *
 * `orgScoped` marks the sections whose rows carry an `org_id`: without the
 * organizations themselves, those rows would land on a server where their
 * organization does not exist, so the server sends only master's. The pane
 * says so rather than letting someone find out from a restore.
 */
const SECTIONS: {
  key: string
  label: string
  hint: string
  defaultOn: boolean
  orgScoped: boolean
}[] = [
  { key: 'tokens', label: 'Access tokens', hint: 'Hashes only, never the secrets', defaultOn: true, orgScoped: true },
  { key: 'webhooks', label: 'Webhooks', hint: 'Endpoints, events and signing secrets', defaultOn: true, orgScoped: true },
  { key: 'users', label: 'Dashboard users', hint: 'Password hashes, TOTP secrets, passkeys', defaultOn: true, orgScoped: true },
  { key: 'organizations', label: 'Organizations', hint: 'Tenants, quotas and hostname allowlists', defaultOn: true, orgScoped: false },
  { key: 'scaling', label: 'Autoscaling', hint: 'Cold-start and scale-out records', defaultOn: true, orgScoped: true },
  { key: 'settings_overrides', label: 'Settings overrides', hint: 'Server settings changed from this dashboard', defaultOn: true, orgScoped: false },
  { key: 'statistics', label: 'Statistics', hint: 'Lifetime counters and the daily, weekly, monthly and yearly buckets', defaultOn: false, orgScoped: true },
  { key: 'uptime', label: 'Uptime history', hint: 'Per-client and per-hostname availability by day', defaultOn: false, orgScoped: true },
  { key: 'inbox', label: 'Webhook inbox', hint: 'Captured inbound webhook payloads', defaultOn: false, orgScoped: true },
  { key: 'admin_keys', label: 'Admin API keys', hint: 'Programmatic admin credentials, hashes only', defaultOn: false, orgScoped: true },
]

/**
 * The whole server as one JSON document, and back again.
 *
 * Its own pane rather than a section of the settings form: it is not a
 * setting, it moves every token, webhook, user and override at once, and
 * importing overwrites all of them. Filed under the form it can replace, it
 * read as one more knob.
 */
export function ExportImportSection() {
  const { t } = useI18n()
  const [busy, setBusy] = useState(false)
  const [note, setNote] = useState<{ ok: boolean; text: string } | null>(null)
  const [chosen, setChosen] = useState<Set<string>>(
    () => new Set(SECTIONS.filter((s) => s.defaultOn).map((s) => s.key)),
  )

  const toggle = (key: string) =>
    setChosen((prev) => {
      const next = new Set(prev)
      if (!next.delete(key)) next.add(key)
      return next
    })

  const orgsIncluded = chosen.has('organizations')
  // What the download will silently leave behind, named while it can still be
  // changed rather than discovered during a restore.
  const orphaned = SECTIONS.filter((s) => s.orgScoped && chosen.has(s.key)).map((s) => t(s.label))

  const download = () => {
    const include = SECTIONS.filter((s) => chosen.has(s.key)).map((s) => s.key)
    window.location.href = `/aperio/api/export?include=${encodeURIComponent(include.join(','))}`
  }

  const importFile = async (file: File) => {
    if (!window.confirm(t('Importing replaces every section the file contains, on this server. Continue?'))) {
      return
    }
    setBusy(true)
    setNote(null)
    try {
      const body = JSON.parse(await file.text())
      const res = await fetch('/aperio/api/import', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(body),
      })
      if (!res.ok) throw new Error(await res.text())
      const out = (await res.json()) as { imported: Record<string, number> }
      const summary = Object.entries(out.imported)
        .map(([k, v]) => `${k}: ${v}`)
        .join(', ')
      setNote({ ok: true, text: `${t('Import applied')} (${summary})` })
    } catch (e) {
      setNote({ ok: false, text: e instanceof Error ? e.message : String(e) })
    } finally {
      setBusy(false)
    }
  }

  return (
    <section className="flex flex-col gap-4">
      <SectionHeader
        title={t('Export & Import')}
        description={t('A logical JSON dump, a failsafe for upgrades and migrations. Choose what travels: the configuration that rebuilds a deployment is on by default, the history is there if you want it. Sessions and the audit log are never exported.')}
      />
      <div className="grid grid-cols-1 gap-2 @2xl:grid-cols-2">
        {SECTIONS.map((s) => (
          <label
            key={s.key}
            className="flex cursor-pointer items-start gap-2.5 rounded-3xl border px-4 py-3"
          >
            <Checkbox
              className="mt-0.5"
              checked={chosen.has(s.key)}
              onCheckedChange={() => toggle(s.key)}
            />
            <span className="flex flex-col gap-0.5">
              <span className="text-sm font-medium">{t(s.label)}</span>
              <span className="text-xs text-muted-foreground">{t(s.hint)}</span>
            </span>
          </label>
        ))}
      </div>
      {!orgsIncluded && orphaned.length > 0 && (
        <p className="rounded-3xl border border-amber-500/30 bg-amber-500/10 px-4 py-3 text-xs">
          {t(
            'Organizations are not selected, so only the master organization travels: {sections} belonging to a tenant are left out, its statistics with them. A row whose organization does not exist on the target server would be an orphan.',
            { sections: orphaned.join(', ') },
          )}
        </p>
      )}
      <div className="flex flex-wrap items-center gap-3">
        <Button variant="outline" onClick={download} disabled={chosen.size === 0}>
          {t('Download export')}
        </Button>
        <Button variant="outline" disabled={busy} onClick={() => {
          const input = document.createElement('input')
          input.type = 'file'
          input.accept = 'application/json,.json'
          input.onchange = () => {
            const file = input.files?.[0]
            if (file) void importFile(file)
          }
          input.click()
        }}>
          {busy && <Spinner />} {t('Import dump…')}
        </Button>
        <span className="text-xs text-muted-foreground">
          {t('An import applies whatever sections the file holds, so it is the export that decides.')}
        </span>
        {note && (
          <span className={cn('text-xs', note.ok ? 'text-emerald-600 dark:text-emerald-400' : 'text-red-600 dark:text-red-400')}>
            {note.text}
          </span>
        )}
      </div>
    </section>
  )
}
