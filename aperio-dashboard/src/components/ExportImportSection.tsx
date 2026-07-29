import { useState } from 'react'
import { SectionHeader } from './shared'
import { Button } from '@/components/ui/button'
import { Spinner } from '@/components/ui/spinner'
import { useI18n } from '@/i18n'
import { cn } from '@/lib/utils'

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

  const importFile = async (file: File) => {
    if (!window.confirm(t('Importing replaces the tokens, webhooks, users and settings of this server. Continue?'))) {
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
        description={t('A logical JSON dump of tokens, webhooks, users and settings overrides — a failsafe for upgrades and migrations. Statistics and sessions are not included.')}
      />
      <div className="flex flex-wrap items-center gap-3">
        <Button variant="outline" onClick={() => { window.location.href = '/aperio/api/export' }}>
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
        {note && (
          <span className={cn('text-xs', note.ok ? 'text-emerald-600 dark:text-emerald-400' : 'text-red-600 dark:text-red-400')}>
            {note.text}
          </span>
        )}
      </div>
    </section>
  )
}
