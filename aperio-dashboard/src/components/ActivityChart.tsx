import { useEffect, useMemo, useState } from 'react'
import { Area, AreaChart, CartesianGrid, XAxis, YAxis } from 'recharts'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'
import {
  ChartContainer,
  ChartTooltip,
  ChartTooltipContent,
  type ChartConfig,
} from '@/components/ui/chart'
import { usePoll } from '@/hooks/usePoll'
import { api } from '@/lib/api'
import { formatCount } from '@/lib/format'
import { useI18n } from '@/i18n'
import { cn } from '@/lib/utils'

const chartConfig = {
  rps: {
    label: 'Requests/s',
    color: 'var(--chart-1)',
  },
} satisfies ChartConfig

/** The two questions this chart answers, and the series each one needs. */
type Range = 'live' | 'long'

/**
 * Requests per second, over the last minute or the last quarter hour.
 *
 * The two ranges come from different places on purpose. **Live** is derived in
 * the browser from the deltas between stats polls: it moves with the poll, so
 * it answers "is it moving right now", and it starts empty on every reload
 * because there is nothing to remember. **Long** is the server's own ring of
 * five-second slices: it survives a reload, it is the same for two people
 * looking at once, and five seconds is the resolution that makes a quarter of
 * an hour readable, a per-second line over that span is noise the eye cannot
 * use.
 */
export function ActivityChart({ history }: { history: number[] }) {
  const { t } = useI18n()
  const [range, setRange] = useState<Range>('live')
  // Only polled while the long view is on screen: the ring is on the server
  // and costs nothing to skip.
  const { data: activity, refresh } = usePoll(
    range === 'long' ? api.activity : async () => null,
    10_000,
  )
  // Switching to the long view asks at once rather than waiting out the poll:
  // ten seconds of an empty chart reads as "no traffic", which is a lie.
  useEffect(() => {
    if (range === 'long') refresh()
  }, [range, refresh])

  const live = useMemo(
    () =>
      history.map((v, i) => ({
        // Sample i is (length - i) polls ago; each poll is ~2 s apart.
        secondsAgo: (history.length - 1 - i) * 2,
        rps: Number(v.toFixed(2)),
      })),
    [history],
  )

  const long = useMemo(() => {
    if (!activity) return []
    const width = activity.bucket_secs || 5
    const newest = activity.buckets[activity.buckets.length - 1]?.at ?? 0
    return activity.buckets.map((b) => ({
      secondsAgo: newest - b.at,
      // A bucket counts requests over its whole width, and the axis is a rate.
      rps: Number((b.total / width).toFixed(2)),
      total: b.total,
      failed: b.failed,
    }))
  }, [activity])

  const data = range === 'live' ? live : long
  const spanSecs = data.length > 0 ? data[0].secondsAgo : 0
  // Over a quarter of an hour the ticks have to be chosen rather than spaced:
  // left to fit by width, recharts lands several of them inside the same
  // minute and the axis reads "-14m -14m -13m".
  const ticks = useMemo(
    () =>
      range === 'long'
        ? Array.from({ length: Math.floor(spanSecs / 60) + 1 }, (_, i) => i * 60).reverse()
        : undefined,
    [range, spanSecs],
  )
  const label = (secondsAgo: number) => {
    if (secondsAgo === 0) return t('now')
    if (secondsAgo < 60) return t('{count}s ago', { count: secondsAgo })
    return t('{count}m ago', { count: Math.round(secondsAgo / 60) })
  }

  return (
    <Card className="py-5">
      <CardHeader className="px-5">
        <div className="flex flex-wrap items-start justify-between gap-2">
          <div className="flex flex-col gap-1">
            <CardTitle className="font-heading text-sm font-semibold uppercase tracking-wider text-muted-foreground">{t('Live Request Activity')}</CardTitle>
            <CardDescription>
              {range === 'live'
                ? t('Requests / second (last 60 seconds)')
                : t('Requests / second, in 5-second slices (last {minutes} minutes)', {
                    minutes: Math.max(1, Math.round(spanSecs / 60)),
                  })}
            </CardDescription>
          </div>
          {/* Two buttons rather than a dropdown: there are two answers, and
              the one you are not looking at is worth naming on screen. */}
          <div className="flex items-center gap-1">
            {(['live', 'long'] as Range[]).map((r) => (
              <Button
                key={r}
                size="xs"
                variant={range === r ? 'secondary' : 'ghost'}
                className={cn('text-xs', range === r && 'font-semibold')}
                onClick={() => setRange(r)}
              >
                {r === 'live' ? t('60 s') : t('15 min')}
              </Button>
            ))}
          </div>
        </div>
      </CardHeader>
      <CardContent className="px-5">
        <ChartContainer config={chartConfig} className="h-36 w-full">
          {/* A right margin only for the long view: its last tick is a word
              ("now") centred on the edge, so half of it falls off. */}
          <AreaChart
            data={data}
            margin={{ left: 0, right: range === 'long' ? 14 : 0, top: 4, bottom: 0 }}
          >
            <defs>
              <linearGradient id="fill-rps" x1="0" y1="0" x2="0" y2="1">
                <stop offset="5%" stopColor="var(--color-rps)" stopOpacity={0.6} />
                <stop offset="95%" stopColor="var(--color-rps)" stopOpacity={0.05} />
              </linearGradient>
            </defs>
            <CartesianGrid vertical={false} strokeDasharray="3 3" />
            <XAxis
              dataKey="secondsAgo"
              reversed={false}
              tickLine={false}
              axisLine={false}
              tickMargin={6}
              ticks={ticks}
              interval={range === 'long' ? 0 : 'preserveStartEnd'}
              tickFormatter={(v: number) =>
                v === 0 ? t('now') : v < 60 ? `-${v}s` : `-${Math.round(v / 60)}m`
              }
            />
            <YAxis hide domain={[0, 'auto']} />
            <ChartTooltip
              cursor={false}
              content={
                <ChartTooltipContent
                  labelFormatter={(_, payload) => {
                    const point = payload?.[0]?.payload as
                      | { secondsAgo: number; total?: number }
                      | undefined
                    if (!point) return ''
                    const when = label(point.secondsAgo)
                    // The long view has the count behind the rate, which is
                    // the number someone is usually after.
                    return point.total === undefined
                      ? when
                      : `${when} · ${t('{count} requests', { count: formatCount(point.total) })}`
                  }}
                  indicator="line"
                />
              }
            />
            <Area
              dataKey="rps"
              type="monotone"
              fill="url(#fill-rps)"
              stroke="var(--color-rps)"
              strokeWidth={2}
              isAnimationActive={false}
            />
          </AreaChart>
        </ChartContainer>
      </CardContent>
    </Card>
  )
}
