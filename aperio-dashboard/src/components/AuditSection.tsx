import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { DownloadIcon, FileTextIcon, RotateCwIcon, XIcon } from 'lucide-react'
import { EmptyRow, SectionHeader, SkeletonRows } from './shared'
import { TintBadge } from './badges'
import { Button } from '@/components/ui/button'
import { Card } from '@/components/ui/card'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table'
import { Tooltip, TooltipContent, TooltipTrigger } from '@/components/ui/tooltip'
import { usePoll } from '@/hooks/usePoll'
import { api, auditQuery, type AuditFilter } from '@/lib/api'
import { formatAbsoluteTime, formatRelativeTime } from '@/lib/format'
import { useI18n } from '@/i18n'

const EMPTY: AuditFilter = { q: '', event: '', actor: '', from: '', to: '' }

const isActive = (f: AuditFilter) =>
  Object.values(f).some((v) => typeof v === 'string' && v.trim() !== '')

export function AuditSection() {
  const { t } = useI18n()
  // The form is edited freely; only an applied filter reaches the server, so
  // typing does not fire a search of the durable log per keystroke.
  const [form, setForm] = useState<AuditFilter>(EMPTY)
  const [applied, setApplied] = useState<AuditFilter>(EMPTY)
  const active = useMemo(() => isActive(applied), [applied])
  const fetchAudit = useCallback(() => api.audit(active ? applied : undefined), [active, applied])
  // A filtered view reads files rather than the in-memory ring, so it is not
  // re-polled every ten seconds underneath the reader; the live unfiltered
  // view still is, and the refresh button works in both.
  const { data: events, refresh } = usePoll(fetchAudit, active ? 300_000 : 10_000)

  // usePoll keeps the fetcher in a ref and only re-runs on an interval change,
  // so applying a different filter has to ask for the new query itself. The
  // effect runs after the render that updated the ref, which is what makes it
  // fetch the new filter rather than the previous one.
  const first = useRef(true)
  useEffect(() => {
    if (first.current) {
      first.current = false
      return
    }
    refresh()
  }, [applied, refresh])

  const set = (key: keyof AuditFilter) => (e: React.ChangeEvent<HTMLInputElement>) =>
    setForm((f) => ({ ...f, [key]: e.target.value }))

  return (
    <section className="flex flex-col gap-3">
      <SectionHeader title={t('Audit Log')}>
        <Tooltip>
          <TooltipTrigger
            render={
              <Button
                size="sm"
                variant="outline"
                render={<a href={`/aperio/api/export/audit.csv${auditQuery(applied)}`} />}
              />
            }
          >
            <DownloadIcon />
            {t('Export CSV')}
          </TooltipTrigger>
          <TooltipContent>{t('Download the matching events as CSV')}</TooltipContent>
        </Tooltip>
        <Tooltip>
          <TooltipTrigger
            render={
              <Button size="icon-sm" variant="outline" onClick={refresh} aria-label={t('Refresh')} />
            }
          >
            <RotateCwIcon />
          </TooltipTrigger>
          <TooltipContent>{t('Refresh')}</TooltipContent>
        </Tooltip>
      </SectionHeader>

      <Card className="flex flex-col gap-3 p-3">
        <form
          className="grid gap-3 sm:grid-cols-2 lg:grid-cols-5"
          onSubmit={(e) => {
            e.preventDefault()
            setApplied(form)
          }}
        >
          <div className="grid gap-1">
            <Label htmlFor="audit-q">{t('Search')}</Label>
            <Input
              id="audit-q"
              value={form.q ?? ''}
              onChange={set('q')}
              placeholder={t('details, event or user')}
            />
          </div>
          <div className="grid gap-1">
            <Label htmlFor="audit-event">{t('Event')}</Label>
            <Input
              id="audit-event"
              value={form.event ?? ''}
              onChange={set('event')}
              placeholder="login_success"
            />
          </div>
          <div className="grid gap-1">
            <Label htmlFor="audit-actor">{t('User')}</Label>
            <Input
              id="audit-actor"
              value={form.actor ?? ''}
              onChange={set('actor')}
              placeholder="aperio"
            />
          </div>
          <div className="grid gap-1">
            <Label htmlFor="audit-from">{t('From')}</Label>
            <Input id="audit-from" type="date" value={form.from ?? ''} onChange={set('from')} />
          </div>
          <div className="grid gap-1">
            <Label htmlFor="audit-to">{t('To')}</Label>
            <Input id="audit-to" type="date" value={form.to ?? ''} onChange={set('to')} />
          </div>
          <div className="flex flex-wrap items-center gap-2 sm:col-span-2 lg:col-span-5">
            <Button type="submit" size="sm">
              {t('Apply filters')}
            </Button>
            {active && (
              <Button
                type="button"
                size="sm"
                variant="outline"
                onClick={() => {
                  setForm(EMPTY)
                  setApplied(EMPTY)
                }}
              >
                <XIcon />
                {t('Clear')}
              </Button>
            )}
            <span className="text-xs text-muted-foreground">
              {active
                ? t('Searching the whole audit log; live updates are paused')
                : t('Showing recent events. Filter to search the whole log.')}
            </span>
          </div>
        </form>
      </Card>

      <Card className="overflow-hidden py-0">
        <Table>
          <TableHeader>
            <TableRow>
              <TableHead>{t('Time')}</TableHead>
              <TableHead>{t('Event')}</TableHead>
              <TableHead>{t('User')}</TableHead>
              <TableHead>{t('Actor IP')}</TableHead>
              <TableHead>{t('Details')}</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {events === null ? (
              <SkeletonRows rows={5} cols={5} />
            ) : events.length === 0 ? (
              <EmptyRow colSpan={5} icon={<FileTextIcon />}>
                {active ? t('No events match these filters') : t('No audit events')}
              </EmptyRow>
            ) : (
              // The ring arrives oldest-first and a search arrives newest-first;
              // the table shows newest first either way.
              (active ? events : [...events].reverse()).map((ev, i) => (
                <TableRow key={`${ev.ts}-${i}`}>
                  <TableCell>
                    <Tooltip>
                      <TooltipTrigger
                        render={<span className="font-mono text-xs text-muted-foreground" />}
                      >
                        {formatRelativeTime(ev.ts)}
                      </TooltipTrigger>
                      <TooltipContent>{formatAbsoluteTime(ev.ts)}</TooltipContent>
                    </Tooltip>
                  </TableCell>
                  <TableCell>
                    <TintBadge tint="gray">{ev.event}</TintBadge>
                  </TableCell>
                  <TableCell>
                    {ev.actor && ev.actor !== '-' ? (
                      <span className="text-sm font-medium">{ev.actor}</span>
                    ) : (
                      <span className="text-muted-foreground">-</span>
                    )}
                  </TableCell>
                  <TableCell>
                    <code className="font-mono text-xs">{ev.actor_ip}</code>
                  </TableCell>
                  <TableCell>
                    <span className="break-all font-mono text-xs">{ev.details}</span>
                  </TableCell>
                </TableRow>
              ))
            )}
          </TableBody>
        </Table>
      </Card>
    </section>
  )
}
