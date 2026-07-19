import { RefreshCwIcon, TurtleIcon } from 'lucide-react'
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
import { useI18n } from '@/i18n'

interface SlowEndpoint {
  host: string
  path: string
  samples: number
  count: number
  errors: number
  avg_ms: number
  p50_ms: number
  p95_ms: number
  max_ms: number
}

/**
 * Top slowest endpoints by recent-window p95 latency — the "where is the
 * time going" table. Fed by `/aperio/api/slow-endpoints` (a rolling
 * in-memory window per host|path).
 */
export function SlowEndpointsSection() {
  const { t } = useI18n()
  const [rows, setRows] = useState<SlowEndpoint[] | null>(null)

  const reload = useCallback(() => {
    fetch('/aperio/api/slow-endpoints')
      .then((r) => r.json())
      .then((data: SlowEndpoint[]) => setRows(data))
      .catch(() => setRows([]))
  }, [])

  useEffect(() => {
    reload()
    const timer = setInterval(reload, 15_000)
    return () => clearInterval(timer)
  }, [reload])

  return (
    <section className="flex flex-col gap-3">
      <SectionHeader title={t('Slowest Endpoints')}>
        <Button size="sm" variant="outline" onClick={reload}>
          <RefreshCwIcon /> {t('Refresh')}
        </Button>
      </SectionHeader>
      <p className="max-w-3xl text-sm text-muted-foreground">
        {t('Recent-window latency per endpoint, worst p95 first — where the time is going right now.')}
      </p>
      <Card className="overflow-hidden py-0">
        <Table>
          <TableHeader>
            <TableRow>
              <TableHead>{t('Hostname')}</TableHead>
              <TableHead>{t('Path')}</TableHead>
              <TableHead className="text-right">p50</TableHead>
              <TableHead className="text-right">p95</TableHead>
              <TableHead className="text-right">{t('Max')}</TableHead>
              <TableHead className="text-right">{t('Requests')}</TableHead>
              <TableHead className="text-right">5xx</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {rows === null ? (
              <SkeletonRows rows={4} cols={7} />
            ) : rows.length === 0 ? (
              <EmptyRow colSpan={7} icon={<TurtleIcon />}>
                {t('Not enough recent traffic yet — the table needs a few requests per endpoint.')}
              </EmptyRow>
            ) : (
              rows.map((r) => (
                <TableRow key={`${r.host}|${r.path}`}>
                  <TableCell className="font-mono text-xs">{r.host}</TableCell>
                  <TableCell>
                    <span className="inline-block max-w-80 break-all font-mono text-sm">{r.path}</span>
                  </TableCell>
                  <TableCell className="text-right font-mono text-sm tabular-nums">
                    {r.p50_ms} ms
                  </TableCell>
                  <TableCell className="text-right font-mono text-sm font-semibold tabular-nums">
                    {r.p95_ms} ms
                  </TableCell>
                  <TableCell className="text-right font-mono text-sm tabular-nums">
                    {r.max_ms} ms
                  </TableCell>
                  <TableCell className="text-right font-mono text-sm tabular-nums">{r.count}</TableCell>
                  <TableCell className="text-right font-mono text-sm tabular-nums">
                    {r.errors > 0 ? (
                      <span className="text-red-500">{r.errors}</span>
                    ) : (
                      <span className="text-muted-foreground">0</span>
                    )}
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
