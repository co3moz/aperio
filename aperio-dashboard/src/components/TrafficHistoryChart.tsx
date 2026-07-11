import { useCallback, useEffect, useMemo, useState } from 'react'
import { Bar, BarChart, CartesianGrid, XAxis, YAxis } from 'recharts'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'
import {
  ChartContainer,
  ChartTooltip,
  ChartTooltipContent,
  type ChartConfig,
} from '@/components/ui/chart'
import { Input } from '@/components/ui/input'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { api, type HistoryBucket } from '@/lib/api'
import { formatBytes } from '@/lib/format'
import { useI18n } from '@/i18n'

/** Rolling-window presets plus a custom day range. */
const RANGES = [
  { key: '7d', unit: 'day', count: 7 },
  { key: '30d', unit: 'day', count: 30 },
  { key: '60d', unit: 'day', count: 60 },
  { key: '26w', unit: 'week', count: 26 },
  { key: '24m', unit: 'month', count: 24 },
  { key: 'custom', unit: 'day', count: 0 },
] as const

type RangeKey = (typeof RANGES)[number]['key']

const chartConfig = {
  success: { label: 'OK', color: 'var(--chart-1)' },
  failed: { label: 'Failed', color: 'var(--chart-5)' },
} satisfies ChartConfig

function isoDaysAgo(days: number): string {
  const d = new Date(Date.now() - days * 86_400_000)
  return d.toISOString().slice(0, 10)
}

/** Bar chart of persisted traffic buckets with a date-range selector. */
export function TrafficHistoryChart() {
  const { t } = useI18n()
  const [range, setRange] = useState<RangeKey>('30d')
  const [from, setFrom] = useState(() => isoDaysAgo(13))
  const [to, setTo] = useState(() => isoDaysAgo(0))
  const [buckets, setBuckets] = useState<HistoryBucket[] | null>(null)
  const [error, setError] = useState<string | null>(null)

  const load = useCallback(async () => {
    setError(null)
    try {
      if (range === 'custom') {
        if (!from || !to) return
        setBuckets(await api.statsHistory({ from, to }))
      } else {
        const preset = RANGES.find((r) => r.key === range) ?? RANGES[1]
        setBuckets(await api.statsHistory({ unit: preset.unit, count: preset.count }))
      }
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e))
    }
  }, [range, from, to])

  useEffect(() => {
    void load()
  }, [load])

  const rangeLabels: Record<RangeKey, string> = useMemo(
    () => ({
      '7d': t('Last 7 days'),
      '30d': t('Last 30 days'),
      '60d': t('Last 60 days'),
      '26w': t('Last 26 weeks'),
      '24m': t('Last 24 months'),
      custom: t('Custom range'),
    }),
    [t],
  )

  const data = (buckets ?? []).map((b) => ({
    period: b.period,
    success: b.success,
    failed: b.failed,
    requests: b.requests,
    bytes: b.bytes_sent + b.bytes_received,
    avg_ms: b.avg_ms,
  }))
  const totals = data.reduce(
    (acc, b) => ({ requests: acc.requests + b.requests, bytes: acc.bytes + b.bytes }),
    { requests: 0, bytes: 0 },
  )

  return (
    <Card className="py-5">
      <CardHeader className="px-5">
        <div className="flex flex-wrap items-start justify-between gap-3">
          <div className="flex flex-col gap-1.5">
            <CardTitle className="font-heading text-sm font-semibold uppercase tracking-wider text-muted-foreground">
              {t('Traffic History')}
            </CardTitle>
            <CardDescription>
              {t('{requests} requests, {bytes} transferred in this range', {
                requests: totals.requests.toLocaleString(),
                bytes: formatBytes(totals.bytes),
              })}
            </CardDescription>
          </div>
          <div className="flex flex-wrap items-center gap-2">
            <Select value={range} onValueChange={(v) => setRange(v as RangeKey)}>
              <SelectTrigger className="w-40" size="sm">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {RANGES.map((r) => (
                  <SelectItem key={r.key} value={r.key}>
                    {rangeLabels[r.key]}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
            {range === 'custom' && (
              <>
                <Input
                  type="date"
                  value={from}
                  max={to}
                  onChange={(e) => setFrom(e.target.value)}
                  className="h-8 w-36"
                  aria-label={t('From date')}
                />
                <Input
                  type="date"
                  value={to}
                  min={from}
                  onChange={(e) => setTo(e.target.value)}
                  className="h-8 w-36"
                  aria-label={t('To date')}
                />
              </>
            )}
          </div>
        </div>
      </CardHeader>
      <CardContent className="px-5">
        {error ? (
          <p className="py-8 text-center text-sm text-destructive">{error}</p>
        ) : (
          <ChartContainer config={chartConfig} className="h-56 w-full">
            <BarChart data={data} margin={{ left: 0, right: 0, top: 4, bottom: 0 }}>
              <CartesianGrid vertical={false} strokeDasharray="3 3" />
              <XAxis
                dataKey="period"
                tickLine={false}
                axisLine={false}
                tickMargin={6}
                interval="preserveStartEnd"
                tickFormatter={(v: string) => v.slice(v.startsWith(String(new Date().getFullYear())) ? 5 : 0)}
              />
              <YAxis hide domain={[0, 'auto']} />
              <ChartTooltip
                cursor={false}
                content={
                  <ChartTooltipContent
                    labelFormatter={(_, payload) => {
                      const p = payload?.[0]?.payload as (typeof data)[number] | undefined
                      if (!p) return ''
                      return `${p.period} · ${formatBytes(p.bytes)} · ${Math.round(p.avg_ms)} ms`
                    }}
                    indicator="line"
                  />
                }
              />
              <Bar dataKey="success" stackId="req" fill="var(--color-success)" isAnimationActive={false} />
              <Bar
                dataKey="failed"
                stackId="req"
                fill="var(--color-failed)"
                radius={[3, 3, 0, 0]}
                isAnimationActive={false}
              />
            </BarChart>
          </ChartContainer>
        )}
      </CardContent>
    </Card>
  )
}
