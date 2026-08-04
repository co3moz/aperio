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

/** The ranges this chart offers. `live` is the browser's own poll history;
 *  the rest are the server's rings. */
type Range = 'live' | '15m' | '2h' | '1d'

/** The server-backed ranges, in the order they are offered.
 *
 * The slice width grows with the span on purpose: every range is about sixty
 * cells, which is what keeps the line readable and the payload small. A day at
 * the fine ring's five-second resolution would be seventeen thousand points
 * drawn into a few hundred pixels. The widths are the server's, this table
 * only names them and decides how often to ask: a ring whose newest slice is
 * fifteen minutes wide has nothing new to say every ten seconds.
 */
const SERVER_RANGES = {
  '15m': { pollMs: 10_000, tickEvery: 60 },
  '2h': { pollMs: 30_000, tickEvery: 15 * 60 },
  '1d': { pollMs: 60_000, tickEvery: 3 * 60 * 60 },
} as const

/**
 * Requests per second, over the last minute, quarter hour, two hours or day.
 *
 * The ranges come from two different places on purpose. **Live** is derived in
 * the browser from the deltas between stats polls: it moves with the poll, so
 * it answers "is it moving right now", and it starts empty on every reload
 * because there is nothing to remember. The rest are the server's own rings:
 * they survive a reload, they are the same for two people looking at once, and
 * the two long ones survive a restart, without which a day-long view would
 * answer "what happened overnight" with a shrug.
 */
export function ActivityChart({ history }: { history: number[] }) {
  const { t } = useI18n()
  const [range, setRange] = useState<Range>('live')
  const server = range === 'live' ? null : SERVER_RANGES[range]
  // Only polled while a server-backed range is on screen: the rings are on the
  // server and cost nothing to skip.
  const { data: activity, refresh } = usePoll(
    range === 'live' ? async () => null : () => api.activity(range),
    server?.pollMs ?? 10_000,
    // The range is the question: switching it must not leave the previous
    // range's series on screen under the new labels, nor let a slow reply to
    // the old one land after the new one and put it back there.
    range,
  )
  // Switching range asks at once rather than waiting out the poll: ten seconds
  // of an empty chart reads as "no traffic", which is a lie.
  useEffect(() => {
    if (range !== 'live') refresh()
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
      // Dividing by the width is what makes the ranges comparable: the same
      // traffic reads the same whether a cell covers five seconds or fifteen
      // minutes.
      rps: Number((b.total / width).toFixed(2)),
      total: b.total,
      failed: b.failed,
    }))
  }, [activity])

  const data = range === 'live' ? live : long
  const spanSecs = data.length > 0 ? data[0].secondsAgo : 0
  // Over a long span the ticks have to be chosen rather than spaced: left to
  // fit by width, recharts lands several of them inside the same minute and
  // the axis reads "-14m -14m -13m".
  const ticks = useMemo(() => {
    if (!server) return undefined
    const step = server.tickEvery
    return Array.from({ length: Math.floor(spanSecs / step) + 1 }, (_, i) => i * step).reverse()
  }, [server, spanSecs])
  const label = (secondsAgo: number) => {
    if (secondsAgo === 0) return t('now')
    if (secondsAgo < 60) return t('{count}s ago', { count: secondsAgo })
    if (secondsAgo < 3600) return t('{count}m ago', { count: Math.round(secondsAgo / 60) })
    return t('{count}h ago', { count: Math.round(secondsAgo / 3600) })
  }
  const description =
    range === 'live'
      ? t('Requests / second (last 60 seconds)')
      : range === '15m'
        ? t('Requests / second, in 5-second slices (last 15 minutes)')
        : range === '2h'
          ? t('Requests / second, in 2-minute slices (last 2 hours)')
          : t('Requests / second, in 15-minute slices (last 24 hours)')

  return (
    <Card className="py-5">
      <CardHeader className="px-5">
        <div className="flex flex-wrap items-start justify-between gap-2">
          <div className="flex flex-col gap-1">
            <CardTitle className="font-heading text-sm font-semibold uppercase tracking-wider text-muted-foreground">{t('Live Request Activity')}</CardTitle>
            <CardDescription>{description}</CardDescription>
          </div>
          {/* Buttons rather than a dropdown: the range you are not looking at
              is worth naming on screen. */}
          <div className="flex items-center gap-1">
            {(['live', '15m', '2h', '1d'] as Range[]).map((r) => (
              <Button
                key={r}
                size="xs"
                variant={range === r ? 'secondary' : 'ghost'}
                className={cn('text-xs', range === r && 'font-semibold')}
                onClick={() => setRange(r)}
              >
                {r === 'live'
                  ? t('60 s')
                  : r === '15m'
                    ? t('15 min')
                    : r === '2h'
                      ? t('2 h')
                      : t('1 d')}
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
            margin={{ left: 0, right: range === 'live' ? 0 : 14, top: 4, bottom: 0 }}
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
              interval={range === 'live' ? 'preserveStartEnd' : 0}
              tickFormatter={(v: number) =>
                v === 0
                  ? t('now')
                  : v < 60
                    ? `-${v}s`
                    : v < 3600
                      ? `-${Math.round(v / 60)}m`
                      : `-${Math.round(v / 3600)}h`
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
