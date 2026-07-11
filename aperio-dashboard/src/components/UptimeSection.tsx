import { EmptyRow, SectionHeader, SkeletonRows } from './shared'
import { TintBadge } from './badges'
import { Card } from '@/components/ui/card'
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
import { api, type UptimeEntry } from '@/lib/api'
import { useI18n } from '@/i18n'

function pct(value: number | null): string {
  if (value === null) return '—'
  return `${value >= 99.995 ? '100' : value.toFixed(2)}%`
}

function pctTint(value: number | null): 'green' | 'amber' | 'red' | 'gray' {
  if (value === null) return 'gray'
  if (value >= 99) return 'green'
  if (value >= 95) return 'amber'
  return 'red'
}

/** One tiny bar per day: green/amber/red by that day's uptime share. */
function DayStrip({ entry }: { entry: UptimeEntry }) {
  const { t } = useI18n()
  return (
    <div className="flex h-4 items-end gap-px">
      {entry.days.map((d) => {
        const total = d.up_secs + d.degraded_secs + d.down_secs
        const share = total > 0 ? d.up_secs / total : 0
        const color =
          total === 0
            ? 'bg-muted'
            : share >= 0.99
              ? 'bg-[var(--chart-1)]'
              : share >= 0.95
                ? 'bg-amber-500'
                : 'bg-destructive'
        return (
          <Tooltip key={d.date}>
            <TooltipTrigger render={<span />}>
              <span className={`inline-block h-4 w-1.5 rounded-[1px] ${color}`} />
            </TooltipTrigger>
            <TooltipContent>
              {d.date}: {total > 0 ? `${(share * 100).toFixed(1)}%` : t('no data')}
            </TooltipContent>
          </Tooltip>
        )
      })}
    </div>
  )
}

function StatusBadge({ status }: { status: UptimeEntry['status'] }) {
  const { t } = useI18n()
  if (status === 'up') return <TintBadge tint="green">{t('up')}</TintBadge>
  if (status === 'degraded') return <TintBadge tint="amber">{t('degraded')}</TintBadge>
  return <TintBadge tint="red">{t('down')}</TintBadge>
}

/** Uptime/SLA table: per-service availability with daily history strips. */
export function UptimeSection() {
  const { t } = useI18n()
  const { data: entries } = usePoll(api.uptime, 15_000)

  return (
    <section className="flex flex-col gap-3">
      <SectionHeader title={t('Uptime')} />
      <Card className="overflow-hidden py-0">
        <Table>
          <TableHeader>
            <TableRow>
              <TableHead>{t('Service')}</TableHead>
              <TableHead>{t('Status')}</TableHead>
              <TableHead className="text-right">{t('Today')}</TableHead>
              <TableHead className="text-right">{t('7 days')}</TableHead>
              <TableHead className="text-right">{t('30 days')}</TableHead>
              <TableHead>{t('Last 30 days')}</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {entries === null ? (
              <SkeletonRows rows={2} cols={6} />
            ) : entries.length === 0 ? (
              <EmptyRow colSpan={6}>{t('No availability history yet')}</EmptyRow>
            ) : (
              entries.map((e) => (
                <TableRow key={e.name}>
                  <TableCell className="font-medium">{e.name}</TableCell>
                  <TableCell>
                    <StatusBadge status={e.status} />
                  </TableCell>
                  <TableCell className="text-right">
                    <TintBadge tint={pctTint(e.pct_today)}>{pct(e.pct_today)}</TintBadge>
                  </TableCell>
                  <TableCell className="text-right">
                    <TintBadge tint={pctTint(e.pct_7d)}>{pct(e.pct_7d)}</TintBadge>
                  </TableCell>
                  <TableCell className="text-right">
                    <TintBadge tint={pctTint(e.pct_30d)}>{pct(e.pct_30d)}</TintBadge>
                  </TableCell>
                  <TableCell>
                    <DayStrip entry={e} />
                  </TableCell>
                </TableRow>
              ))
            )}
          </TableBody>
        </Table>
      </Card>
      <p className="text-xs text-muted-foreground">
        {t('Percentages cover observed time only — time while the server itself was offline is not counted against a service.')}
      </p>
    </section>
  )
}
