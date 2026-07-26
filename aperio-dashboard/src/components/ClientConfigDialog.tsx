import { useEffect, useState } from 'react'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { Spinner } from '@/components/ui/spinner'
import { TintBadge } from './badges'
import { api, ApiError, type ClientConfigView } from '@/lib/api'
import { useI18n } from '@/i18n'

/** A line of the rendered document plus whether it carries an adjustment. */
interface YamlLine {
  text: string
  /** True for a line the server annotated with a `# declared` comment: the
   *  effective value is not what the config asked for. */
  adjusted: boolean
  comment: boolean
}

/** Splits the document into lines and marks the annotated ones, so a mismatch
 *  is visible at the value itself rather than only in the list below. */
function parseYaml(yaml: string): YamlLine[] {
  return yaml.split('\n').map((text) => ({
    text,
    adjusted: text.includes('# declared '),
    comment: text.trimStart().startsWith('#'),
  }))
}

/** Read-only view of one connection's effective configuration: the YAML the
 *  server assembles from what the client announced, with every setting that
 *  resolved to something other than the configured value called out. */
export function ClientConfigDialog({
  clientId,
  label,
  open,
  onOpenChange,
}: {
  clientId: string
  /** Human-facing name of the connection, for the dialog title. */
  label: string
  open: boolean
  onOpenChange: (open: boolean) => void
}) {
  const { t } = useI18n()
  const [view, setView] = useState<ClientConfigView | null>(null)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    if (!open) return
    let cancelled = false
    setView(null)
    setError(null)
    api
      .clientConfig(clientId)
      .then((v) => {
        if (!cancelled) setView(v)
      })
      .catch((e) => {
        if (!cancelled) setError(e instanceof ApiError ? e.message : String(e))
      })
    return () => {
      cancelled = true
    }
  }, [open, clientId])

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-3xl">
        <DialogHeader>
          <DialogTitle>{t('Configuration of {label}', { label })}</DialogTitle>
          <DialogDescription>
            {t('What this connection announces over its heartbeat, plus what the server applies on top. Settings a client never announces (its target, timeouts, header rules, health probes) cannot be shown here.')}
          </DialogDescription>
        </DialogHeader>
        {error && <p className="text-sm text-destructive">{error}</p>}
        {!view && !error && (
          <p className="flex items-center gap-2 text-sm text-muted-foreground">
            <Spinner /> {t('Loading…')}
          </p>
        )}
        {view && (
          <div className="flex flex-col gap-4">
            <pre className="max-h-96 overflow-auto rounded-md border bg-muted/40 p-3 text-xs leading-relaxed">
              {parseYaml(view.yaml).map((line, i) => (
                <div
                  key={i}
                  className={
                    line.adjusted
                      ? 'rounded-sm bg-amber-500/15 text-amber-700 dark:text-amber-400'
                      : line.comment
                        ? 'text-muted-foreground'
                        : undefined
                  }
                >
                  {line.text || ' '}
                </div>
              ))}
            </pre>
            {view.notes.length > 0 && (
              <div className="flex flex-col gap-2">
                <h3 className="text-sm font-medium">
                  {t('{count} setting(s) differ from the configuration', {
                    count: view.notes.length,
                  })}
                </h3>
                {view.notes.map((note) => (
                  <div
                    key={`${note.field}-${note.source}`}
                    className="flex flex-col gap-1 rounded-md border p-2 text-xs"
                  >
                    <div className="flex flex-wrap items-center gap-2">
                      <span className="font-mono font-medium">{note.field}</span>
                      <TintBadge tint="gray">
                        {note.declared || t('not set')}
                      </TintBadge>
                      <span className="text-muted-foreground">{'->'}</span>
                      <TintBadge tint="amber">{note.effective}</TintBadge>
                      <TintBadge tint={note.source === 'client' ? 'blue' : 'lime'}>
                        {note.source === 'client'
                          ? t('resolved by the client')
                          : t('applied by the server')}
                      </TintBadge>
                    </div>
                    <p className="text-muted-foreground">{note.reason}</p>
                  </div>
                ))}
              </div>
            )}
          </div>
        )}
      </DialogContent>
    </Dialog>
  )
}
