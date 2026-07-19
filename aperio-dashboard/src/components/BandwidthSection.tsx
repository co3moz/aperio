import { GaugeIcon, RefreshCwIcon } from 'lucide-react'
import { useCallback, useEffect, useState } from 'react'
import { EmptyRow, SectionHeader, SkeletonRows } from './shared'
import { Button } from '@/components/ui/button'
import { Card } from '@/components/ui/card'
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table'
import { formatBytes } from '@/lib/format'
import { cn } from '@/lib/utils'
import { useI18n } from '@/i18n'

interface BandwidthBucket {
  period: string
  bytes_sent: number
  bytes_received: number
  requests: number
}

interface BandwidthRow {
  label: string
  total_bytes: number
  buckets: BandwidthBucket[]
}

interface BandwidthReport {
  unit: string
  periods: string[]
  by_token: BandwidthRow[]
  by_hostname: BandwidthRow[]
}

/** Strips the kind prefix from a period key for column headers. */
function periodLabel(key: string): string {
  const raw = key.slice(2)
  // Days render as MM-DD; months as YYYY-MM.
  return key.startsWith('d:') ? raw.slice(5) : raw
}

function BandwidthTable({
  title,
  rows,
  periods,
}: {
  title: string
  rows: BandwidthRow[]
  periods: string[]
}) {
  const { t } = useI18n()
  // Wide period sets scroll horizontally inside the card.
  return (
    <Card className="overflow-x-auto py-0">
      <Table>
        <TableHeader>
          <TableRow>
            <TableHead>{title}</TableHead>
            {periods.map((p) => (
              <TableHead key={p} className="text-right font-mono text-xs">
                {periodLabel(p)}
              </TableHead>
            ))}
            <TableHead className="text-right">{t('Total')}</TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          {rows.length === 0 ? (
            <EmptyRow colSpan={periods.length + 2} icon={<GaugeIcon />}>
              {t('No traffic recorded for this window yet.')}
            </EmptyRow>
          ) : (
            rows.map((row) => (
              <TableRow key={row.label}>
                <TableCell className="font-mono text-xs">{row.label}</TableCell>
                {row.buckets.map((b) => {
                  const total = b.bytes_sent + b.bytes_received
                  return (
                    <TableCell
                      key={b.period}
                      className="text-right font-mono text-xs tabular-nums"
                      title={`▲ ${formatBytes(b.bytes_sent)} · ▼ ${formatBytes(b.bytes_received)} · ${b.requests} req`}
                    >
                      {total > 0 ? formatBytes(total) : <span className="text-muted-foreground">—</span>}
                    </TableCell>
                  )
                })}
                <TableCell className="text-right font-mono text-xs font-semibold tabular-nums">
                  {formatBytes(row.total_bytes)}
                </TableCell>
              </TableRow>
            ))
          )}
        </TableBody>
      </Table>
    </Card>
  )
}

/**
 * Bandwidth accounting: bytes per token and per hostname bucketed per day or
 * month — the billing-style view of `/aperio/api/bandwidth`. Cell tooltips
 * split each bucket into sent/received bytes and request counts.
 */
export function BandwidthSection() {
  const { t } = useI18n()
  const [unit, setUnit] = useState<'day' | 'month'>('day')
  const [report, setReport] = useState<BandwidthReport | null>(null)

  const reload = useCallback(() => {
    fetch(`/aperio/api/bandwidth?unit=${unit}`)
      .then((r) => r.json())
      .then((data: BandwidthReport) => setReport(data))
      .catch(() => setReport(null))
  }, [unit])

  useEffect(() => {
    setReport(null)
    reload()
  }, [reload])

  return (
    <section className="flex flex-col gap-3">
      <SectionHeader title={t('Bandwidth')}>
        {(['day', 'month'] as const).map((u) => (
          <Button
            key={u}
            size="sm"
            variant="outline"
            data-active={unit === u}
            className={cn('data-[active=true]:bg-primary/15 data-[active=true]:text-primary')}
            onClick={() => setUnit(u)}
          >
            {u === 'day' ? t('Daily') : t('Monthly')}
          </Button>
        ))}
        <Button size="sm" variant="outline" onClick={reload}>
          <RefreshCwIcon /> {t('Refresh')}
        </Button>
      </SectionHeader>
      <p className="max-w-3xl text-sm text-muted-foreground">
        {t('Bytes through the tunnel per token and hostname — hover a cell for the sent/received split.')}
      </p>
      {report === null ? (
        <Card className="overflow-hidden py-0">
          <Table>
            <TableBody>
              <SkeletonRows rows={3} cols={6} />
            </TableBody>
          </Table>
        </Card>
      ) : (
        <>
          <BandwidthTable title={t('Token')} rows={report.by_token} periods={report.periods} />
          <BandwidthTable
            title={t('Hostname')}
            rows={report.by_hostname}
            periods={report.periods}
          />
        </>
      )}
    </section>
  )
}
