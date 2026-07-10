import { Area, AreaChart, CartesianGrid, XAxis, YAxis } from 'recharts'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'
import {
  ChartContainer,
  ChartTooltip,
  ChartTooltipContent,
  type ChartConfig,
} from '@/components/ui/chart'
import { useI18n } from '@/i18n'

const chartConfig = {
  rps: {
    label: 'Requests/s',
    color: 'var(--chart-1)',
  },
} satisfies ChartConfig

/** Area chart of requests/second over the last minute (recharts). */
export function ActivityChart({ history }: { history: number[] }) {
  const { t } = useI18n()
  const data = history.map((v, i) => ({
    // Sample i is (length - i) polls ago; each poll is ~2 s apart.
    secondsAgo: (history.length - 1 - i) * 2,
    rps: Number(v.toFixed(2)),
  }))

  return (
    <Card className="py-5">
      <CardHeader className="px-5">
        <CardTitle className="font-heading text-sm font-semibold uppercase tracking-wider text-muted-foreground">{t('Live Request Activity')}</CardTitle>
        <CardDescription>{t('Requests / second (last 60 seconds)')}</CardDescription>
      </CardHeader>
      <CardContent className="px-5">
        <ChartContainer config={chartConfig} className="h-36 w-full">
          <AreaChart data={data} margin={{ left: 0, right: 0, top: 4, bottom: 0 }}>
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
              interval="preserveStartEnd"
              tickFormatter={(v: number) => (v === 0 ? 'now' : `-${v}s`)}
            />
            <YAxis hide domain={[0, 'auto']} />
            <ChartTooltip
              cursor={false}
              content={
                <ChartTooltipContent
                  labelFormatter={(_, payload) => {
                    const s = payload?.[0]?.payload?.secondsAgo as number | undefined
                    return s === 0 ? 'now' : `${s}s ago`
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
