import { Card } from '@/components/ui/card'
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table'
import { TintBadge } from './badges'
import { EmptyRow, SectionHeader, SkeletonRows } from './shared'
import { usePoll } from '@/hooks/usePoll'
import { api } from '@/lib/api'
import { useI18n } from '@/i18n'

function fmtUs(us: number | null | undefined): string {
  if (us === null || us === undefined) return '-'
  if (us >= 1_000_000) return `${(us / 1_000_000).toFixed(2)} s`
  if (us >= 1_000) return `${(us / 1_000).toFixed(2)} ms`
  return `${us} µs`
}

const STAGE_LABELS: Record<string, string> = {
  queue: 'queued & routed',
  transit_out: 'tunnel → client',
  client_processing: 'client processing',
  backend_wait: 'backend wait (first byte)',
  backend_body: 'backend body',
  transit_back: 'tunnel → server',
  serve: 'server → visitor',
}

/** Rolling per-stage latency statistics per route, with anomaly flags: a
 *  stage whose latest sample sits far outside its normal band is called out,
 *  so "requests usually queue +5-10ms, now +25-30ms" is visible per stage. */
export function StageStatsSection() {
  const { t } = useI18n()
  const { data: routes } = usePoll(api.stageStats, 5_000)

  return (
    <section className="flex flex-col gap-3">
      <SectionHeader
        title={t('Stage latencies')}
        description={t('Mean and spread of each request stage (rolling window per route, buffered requests of timing-aware clients). A stage far outside its usual band is flagged.')}
      />
      <Card className="overflow-hidden py-0">
        <Table>
          <TableHeader>
            <TableRow>
              <TableHead>{t('Route')}</TableHead>
              <TableHead>{t('Stage')}</TableHead>
              <TableHead className="text-right">{t('Mean')}</TableHead>
              <TableHead className="text-right">σ</TableHead>
              <TableHead className="text-right">{t('Last')}</TableHead>
              <TableHead>{t('Status')}</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {routes === null ? (
              <SkeletonRows rows={4} cols={6} />
            ) : routes.length === 0 ? (
              <EmptyRow colSpan={6}>{t('No timing data yet')}</EmptyRow>
            ) : (
              routes.flatMap((r) =>
                r.stages
                  .filter((s) => s.count > 0)
                  .map((s, i) => (
                    <TableRow key={`${r.host}-${s.stage}`}>
                      <TableCell className="font-mono text-xs">{i === 0 ? r.host : ''}</TableCell>
                      <TableCell>{t(STAGE_LABELS[s.stage] ?? s.stage)}</TableCell>
                      <TableCell className="text-right font-mono tabular-nums text-xs">
                        {fmtUs(s.mean_us)}
                      </TableCell>
                      <TableCell className="text-right font-mono tabular-nums text-xs">
                        {fmtUs(s.stddev_us)}
                      </TableCell>
                      <TableCell className="text-right font-mono tabular-nums text-xs">
                        {fmtUs(s.last_us)}
                      </TableCell>
                      <TableCell>
                        {s.anomalous ? (
                          <TintBadge tint="red">{t('anomaly')}</TintBadge>
                        ) : (
                          <TintBadge tint="green">{t('normal')}</TintBadge>
                        )}
                      </TableCell>
                    </TableRow>
                  )),
              )
            )}
          </TableBody>
        </Table>
      </Card>
    </section>
  )
}
