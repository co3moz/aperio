import {
  BarChartIcon,
  CalendarIcon,
  GlobeIcon,
  LayersIcon,
  PaperPlaneIcon,
  TimerIcon,
} from '@radix-ui/react-icons'
import { Card, Flex, Grid, Skeleton, Text } from '@radix-ui/themes'
import type { ReactNode } from 'react'
import type { ServerStats } from '../lib/api'
import { formatBytes } from '../lib/format'

function StatCard({
  icon,
  title,
  value,
  sub,
  loading,
}: {
  icon: ReactNode
  title: string
  value: string
  sub: string
  loading: boolean
}) {
  return (
    <Card size="3">
      <Flex align="center" gap="2" mb="1">
        <Text color="gray">{icon}</Text>
        <Text size="1" weight="bold" color="gray" style={{ textTransform: 'uppercase', letterSpacing: '1px' }}>
          {title}
        </Text>
      </Flex>
      <Text as="div" size="8" weight="bold">
        <Skeleton loading={loading}>{value}</Skeleton>
      </Text>
      <Text as="div" size="1" color="gray" mt="1">
        <Skeleton loading={loading}>{sub}</Skeleton>
      </Text>
    </Card>
  )
}

export function StatsCards({ stats }: { stats: ServerStats | null }) {
  const s = stats
  const loading = s === null
  const total = s ? s.successful_requests + s.failed_requests : 0
  return (
    <Grid columns={{ initial: '1', xs: '2', md: '3' }} gap="4">
      <StatCard
        loading={loading}
        icon={<GlobeIcon />}
        title="Tunnel Clients"
        value={String(s?.connected_clients_count ?? 0)}
        sub={
          s && s.connected_clients_count > 0
            ? `${s.connected_clients_count} tunnel client(s) active`
            : 'No active web socket client'
        }
      />
      <StatCard
        loading={loading}
        icon={<LayersIcon />}
        title="Queue Status"
        value={String(s?.pending_requests_count ?? 0)}
        sub="Requests pending reconnection"
      />
      <StatCard
        loading={loading}
        icon={<BarChartIcon />}
        title="Total Requests"
        value={String(s?.total_requests ?? 0)}
        sub={`${s?.successful_requests ?? 0} of ${total} successful`}
      />
      <StatCard
        loading={loading}
        icon={<PaperPlaneIcon />}
        title="Data Transferred"
        value={formatBytes(s?.total_bytes_transferred ?? 0)}
        sub="Payload bytes transferred"
      />
      <StatCard
        loading={loading}
        icon={<TimerIcon />}
        title="Avg Response"
        value={s && s.persistent.total_requests > 0 ? `${s.avg_response_ms.toFixed(1)} ms` : '—'}
        sub={
          s
            ? `${s.persistent.total_requests} lifetime requests • ${formatBytes(s.persistent.total_bytes_sent)} sent`
            : 'All-time (persisted)'
        }
      />
      <StatCard
        loading={loading}
        icon={<CalendarIcon />}
        title="Today"
        value={String(s?.today.requests ?? 0)}
        sub={
          s
            ? `${s.today.success} ok / ${s.today.failed} failed • ${formatBytes(s.today.bytes_sent)} sent today`
            : 'Requests today'
        }
      />
    </Grid>
  )
}
