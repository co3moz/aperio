import { TriangleAlertIcon, XIcon } from 'lucide-react'
import { useState, type FormEvent } from 'react'
import { toast } from 'sonner'
import { SectionHeader } from './shared'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import { Input } from '@/components/ui/input'
import { Spinner } from '@/components/ui/spinner'
import { Tooltip, TooltipContent, TooltipTrigger } from '@/components/ui/tooltip'
import { usePoll } from '@/hooks/usePoll'
import { api, ApiError } from '@/lib/api'
import { useI18n } from '@/i18n'

/**
 * Per-hostname maintenance switch: listed hostnames answer with the 503
 * maintenance page even while their tunnel clients stay connected. `*`
 * covers every hostname. In-memory only — a server restart clears it.
 */
export function MaintenanceSection() {
  const { t } = useI18n()
  const { data: hosts, refresh } = usePoll(api.maintenance, 10_000)
  const [hostname, setHostname] = useState('')
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)

  const enable = async (e: FormEvent) => {
    e.preventDefault()
    const value = hostname.trim()
    if (!value) return
    setBusy(true)
    setError(null)
    try {
      await api.setMaintenance(value, true)
      setHostname('')
      toast.info(t('Maintenance enabled for {host}', { host: value }))
      refresh()
    } catch (err) {
      setError(err instanceof ApiError ? err.message : String(err))
    } finally {
      setBusy(false)
    }
  }

  const disable = async (host: string) => {
    try {
      await api.setMaintenance(host, false)
      toast.info(t('Maintenance ended for {host}', { host }))
    } finally {
      refresh()
    }
  }

  return (
    <section className="flex flex-col gap-3">
      <SectionHeader title={t('Maintenance Mode')} />
      <Card className="py-5">
        <CardContent className="flex flex-col gap-4 px-5">
          <form onSubmit={enable} className="flex flex-wrap items-center gap-2">
            <Input
              value={hostname}
              onChange={(e) => setHostname(e.target.value)}
              placeholder={t('app.example.com  (* = all hostnames)')}
              className="max-w-xs"
            />
            <Button
              type="submit"
              variant="outline"
              disabled={busy}
              className="text-amber-700 dark:text-amber-400"
            >
              {busy ? <Spinner /> : <TriangleAlertIcon />} {t('Enable maintenance')}
            </Button>
          </form>
          {error && <p className="text-sm text-destructive">{error}</p>}
          {!hosts || hosts.length === 0 ? (
            <p className="text-sm text-muted-foreground">
              {t('No hostnames in maintenance. Visitors of a listed hostname get the 503 page while its clients stay connected; cleared on server restart.')}
            </p>
          ) : (
            <div className="flex flex-wrap gap-2">
              {hosts.map((h) => (
                <span
                  key={h}
                  className="inline-flex items-center gap-1.5 rounded-full border border-transparent bg-amber-500/15 py-1 pl-3 pr-1 text-sm font-medium text-amber-700 dark:text-amber-400"
                >
                  {h === '*' ? t('* (all hostnames)') : h}
                  <Tooltip>
                    <TooltipTrigger
                      render={
                        <Button
                          size="icon-xs"
                          variant="ghost"
                          className="rounded-full text-amber-700 hover:bg-amber-500/20 dark:text-amber-400"
                          onClick={() => void disable(h)}
                          aria-label={t('End maintenance for {host}', { host: h })}
                        />
                      }
                    >
                      <XIcon />
                    </TooltipTrigger>
                    <TooltipContent>{t('End maintenance')}</TooltipContent>
                  </Tooltip>
                </span>
              ))}
            </div>
          )}
        </CardContent>
      </Card>
    </section>
  )
}
