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
import { api, ApiError, type MaintenanceEntry } from '@/lib/api'
import { formatAbsoluteTime, formatRelativeTime, formatTimeUntil } from '@/lib/format'
import { useI18n } from '@/i18n'
import { useHasRole } from '@/lib/session'

/** Windows offered for a flag, plus the open-ended default. */
const WINDOWS: { label: string; minutes: number | null }[] = [
  { label: 'until turned off', minutes: null },
  { label: '15 min', minutes: 15 },
  { label: '1 h', minutes: 60 },
  { label: '4 h', minutes: 240 },
]

/**
 * Per-hostname maintenance switch: listed hostnames answer with the 503
 * maintenance page even while their tunnel clients stay connected.
 * `*.example.com` covers every subdomain of a domain and `*` covers every
 * hostname on the server. In-memory only, a server restart clears it.
 *
 * A flag carries a reason and, optionally, a window: the flag that causes an
 * outage is the one switched on for twenty minutes of work and forgotten, and
 * the reason is the sentence the visitor and the next operator both read.
 */
export function MaintenanceSection() {
  const { t } = useI18n()
  const canMutate = useHasRole('operator')
  const { data: flags, refresh } = usePoll(api.maintenance, 10_000)
  const [hostname, setHostname] = useState('')
  const [reason, setReason] = useState('')
  const [minutes, setMinutes] = useState<number | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)

  const enable = async (e: FormEvent) => {
    e.preventDefault()
    const value = hostname.trim()
    if (!value) return
    setBusy(true)
    setError(null)
    try {
      await api.setMaintenance(value, true, {
        reason: reason.trim() || undefined,
        ttl_seconds: minutes ? minutes * 60 : undefined,
      })
      setHostname('')
      setReason('')
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
    } catch (err) {
      toast.error(err instanceof ApiError ? err.message : String(err))
    } finally {
      refresh()
    }
  }

  const label = (f: MaintenanceEntry) => (f.hostname === '*' ? t('* (all hostnames)') : f.hostname)

  return (
    <section className="flex flex-col gap-3">
      <SectionHeader title={t('Maintenance Mode')} />
      <Card className="py-5">
        <CardContent className="flex flex-col gap-4 px-5">
          {canMutate && (
          <form onSubmit={enable} className="flex flex-wrap items-center gap-2">
            <Input
              value={hostname}
              onChange={(e) => setHostname(e.target.value)}
              placeholder={t('app.example.com, *.example.com, *-pi.example.com, or *')}
              className="max-w-xs"
            />
            <Input
              value={reason}
              onChange={(e) => setReason(e.target.value)}
              placeholder={t('reason, shown on the 503 page (optional)')}
              className="max-w-xs"
            />
            {/* A window is a fixed list rather than a free number: the mistake
                worth preventing is a duration field holding a timestamp, and
                the useful values are few. */}
            <select
              value={minutes ?? ''}
              onChange={(e) => setMinutes(e.target.value ? Number(e.target.value) : null)}
              className="h-9 rounded-3xl border bg-transparent px-3 text-sm"
              aria-label={t('Maintenance window')}
            >
              {WINDOWS.map((w) => (
                <option key={w.label} value={w.minutes ?? ''}>
                  {t(w.label)}
                </option>
              ))}
            </select>
            <Button
              type="submit"
              variant="outline"
              disabled={busy}
              className="text-amber-700 dark:text-amber-400"
            >
              {busy ? <Spinner /> : <TriangleAlertIcon />} {t('Enable maintenance')}
            </Button>
          </form>
          )}
          {error && <p className="text-sm text-destructive">{error}</p>}
          {!flags || flags.length === 0 ? (
            <p className="text-sm text-muted-foreground">
              {t('No hostnames in maintenance. Visitors of a listed hostname get the 503 page while its clients stay connected; cleared on server restart. Use *.example.com for every subdomain of a domain, and list the domain itself too if you want it as well.')}
            </p>
          ) : (
            <div className="flex flex-col gap-2">
              {flags.map((f) => (
                <div
                  key={f.hostname}
                  className="flex flex-wrap items-center gap-x-3 gap-y-1 rounded-3xl border border-amber-500/30 bg-amber-500/10 py-2 pl-4 pr-2 text-sm"
                >
                  <span className="font-medium text-amber-700 dark:text-amber-400">{label(f)}</span>
                  {f.hostname.startsWith('*.') && (
                    <span className="text-xs text-muted-foreground">{t('every subdomain')}</span>
                  )}
                  {f.reason && <span className="text-muted-foreground">{f.reason}</span>}
                  <span className="ml-auto flex items-center gap-2 text-xs text-muted-foreground">
                    {f.until && formatTimeUntil(f.until) ? (
                      <Tooltip>
                        <TooltipTrigger render={<span />}>
                          {t('lifts in {duration}', { duration: formatTimeUntil(f.until) })}
                        </TooltipTrigger>
                        <TooltipContent>{formatAbsoluteTime(f.until)}</TooltipContent>
                      </Tooltip>
                    ) : (
                      <span>{t('until turned off')}</span>
                    )}
                    <span>
                      {t('set by {actor} {when}', {
                        actor: f.actor,
                        when: formatRelativeTime(f.since),
                      })}
                    </span>
                    {canMutate && (
                      <Tooltip>
                        <TooltipTrigger
                          render={
                            <Button
                              size="icon-xs"
                              variant="ghost"
                              className="rounded-full text-amber-700 hover:bg-amber-500/20 dark:text-amber-400"
                              onClick={() => void disable(f.hostname)}
                              aria-label={t('End maintenance for {host}', { host: f.hostname })}
                            />
                          }
                        >
                          <XIcon />
                        </TooltipTrigger>
                        <TooltipContent>{t('End maintenance')}</TooltipContent>
                      </Tooltip>
                    )}
                  </span>
                </div>
              ))}
            </div>
          )}
        </CardContent>
      </Card>
    </section>
  )
}
