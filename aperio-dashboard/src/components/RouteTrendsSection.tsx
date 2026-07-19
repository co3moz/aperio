import { RefreshCwIcon, TrendingUpIcon } from 'lucide-react'
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
import { Tooltip, TooltipContent, TooltipTrigger } from '@/components/ui/tooltip'
import { useI18n } from '@/i18n'

interface TrendBucket {
  total: number
  s2xx: number
  s3xx: number
  s4xx: number
  s5xx: number
}

interface RouteTrend {
  host: string
  total: number
  error_rate: number
  buckets: TrendBucket[]
}

/** One minute-bar: height by volume, color by the worst status class seen. */
function barColor(b: TrendBucket): string {
  if (b.s5xx > 0) return 'fill-red-500'
  if (b.s4xx > 0) return 'fill-amber-500'
  if (b.total > 0) return 'fill-emerald-500'
  return 'fill-muted'
}

function Sparkline({ buckets }: { buckets: TrendBucket[] }) {
  const max = Math.max(1, ...buckets.map((b) => b.total))
  const barW = 6
  const gap = 2
  const height = 28
  const width = buckets.length * (barW + gap)
  return (
    <svg
      width={width}
      height={height}
      viewBox={`0 0 ${width} ${height}`}
      role="img"
      aria-label="status trend"
    >
      {buckets.map((b, i) => {
        const h = b.total > 0 ? Math.max(3, (b.total / max) * height) : 2
        return (
          <rect
            key={i}
            x={i * (barW + gap)}
            y={height - h}
            width={barW}
            height={h}
            rx={1}
            className={b.total > 0 ? barColor(b) : 'fill-muted-foreground/20'}
          />
        )
      })}
    </svg>
  )
}

/**
 * Per-route status trends: one bar per minute over the last 30 minutes,
 * colored by the worst status class seen in that minute — a glanceable
 * "which route started erroring, and when". Fed by `/aperio/api/route-trends`.
 */
export function RouteTrendsSection() {
  const { t } = useI18n()
  const [routes, setRoutes] = useState<RouteTrend[] | null>(null)

  const reload = useCallback(() => {
    fetch('/aperio/api/route-trends')
      .then((r) => r.json())
      .then((data: RouteTrend[]) => setRoutes(data))
      .catch(() => setRoutes([]))
  }, [])

  useEffect(() => {
    reload()
    const timer = setInterval(reload, 15_000)
    return () => clearInterval(timer)
  }, [reload])

  return (
    <section className="flex flex-col gap-3">
      <SectionHeader title={t('Route Trends')}>
        <Button size="sm" variant="outline" onClick={reload}>
          <RefreshCwIcon /> {t('Refresh')}
        </Button>
      </SectionHeader>
      <p className="max-w-3xl text-sm text-muted-foreground">
        {t('One bar per minute over the last 30 minutes, colored by the worst status class — spot which route started erroring, and when.')}
      </p>
      <Card className="overflow-x-auto py-0">
        <Table>
          <TableHeader>
            <TableRow>
              <TableHead>{t('Hostname')}</TableHead>
              <TableHead>{t('Last 30 minutes')}</TableHead>
              <TableHead className="text-right">{t('Requests')}</TableHead>
              <TableHead className="text-right">{t('Error rate')}</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {routes === null ? (
              <SkeletonRows rows={3} cols={4} />
            ) : routes.length === 0 ? (
              <EmptyRow colSpan={4} icon={<TrendingUpIcon />}>
                {t('No traffic in the last 30 minutes.')}
              </EmptyRow>
            ) : (
              routes.map((r) => (
                <TableRow key={r.host}>
                  <TableCell className="font-mono text-xs">{r.host}</TableCell>
                  <TableCell>
                    <Tooltip>
                      <TooltipTrigger render={<span className="inline-block" />}>
                        <Sparkline buckets={r.buckets} />
                      </TooltipTrigger>
                      <TooltipContent>
                        {t('green = 2xx/3xx, amber = 4xx seen, red = 5xx seen')}
                      </TooltipContent>
                    </Tooltip>
                  </TableCell>
                  <TableCell className="text-right font-mono text-sm tabular-nums">
                    {r.total}
                  </TableCell>
                  <TableCell className="text-right font-mono text-sm tabular-nums">
                    <span className={r.error_rate > 0 ? 'text-red-500' : 'text-muted-foreground'}>
                      {(r.error_rate * 100).toFixed(1)}%
                    </span>
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
