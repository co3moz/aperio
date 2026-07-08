import { Card, Flex, Text } from '@radix-ui/themes'

const W = 1000
const H = 120

/** SVG sparkline of requests/second over the last minute. */
export function ActivityChart({ history }: { history: number[] }) {
  const max = Math.max(...history, 1)
  const stepX = W / (history.length - 1)
  const points = history
    .map((v, i) => `${(i * stepX).toFixed(1)},${(H - (v / max) * (H - 24) - 12).toFixed(1)}`)
    .join(' ')
  const area = `${points} ${W},${H} 0,${H}`

  return (
    <Card size="3">
      <Flex justify="between" align="center" mb="3">
        <Text size="1" weight="bold" color="gray" style={{ textTransform: 'uppercase', letterSpacing: '1px' }}>
          Live Request Activity
        </Text>
        <Text size="1" color="gray">
          Requests / second (last 60 seconds)
        </Text>
      </Flex>
      <svg
        viewBox={`0 0 ${W} ${H}`}
        preserveAspectRatio="none"
        style={{ width: '100%', height: 120, display: 'block' }}
      >
        <defs>
          <linearGradient id="activity-fill" x1="0" y1="0" x2="0" y2="1">
            <stop offset="0%" stopColor="var(--indigo-9)" stopOpacity="0.25" />
            <stop offset="100%" stopColor="var(--indigo-9)" stopOpacity="0" />
          </linearGradient>
        </defs>
        {[0, 1, 2, 3].map((i) => (
          <line
            key={i}
            x1="0"
            x2={W}
            y1={(H * i) / 3}
            y2={(H * i) / 3}
            stroke="var(--gray-a4)"
            strokeWidth="1"
          />
        ))}
        <polygon points={area} fill="url(#activity-fill)" />
        <polyline
          points={points}
          fill="none"
          stroke="var(--indigo-9)"
          strokeWidth="2.5"
          vectorEffect="non-scaling-stroke"
        />
      </svg>
    </Card>
  )
}
