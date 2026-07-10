import {
  ArrowLeftRightIcon,
  CalendarDaysIcon,
  GaugeIcon,
  GlobeIcon,
  LayersIcon,
  TrendingUpIcon,
} from 'lucide-react'
import type { ReactNode } from 'react'
import { Card, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'
import { Skeleton } from '@/components/ui/skeleton'
import type { ServerStats } from '@/lib/api'
import { formatBytes } from '@/lib/format'
import { useI18n } from '@/i18n'

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
    <Card className="gap-2 py-5">
      <CardHeader className="px-5">
        <CardDescription className="flex items-center gap-2 text-xs font-medium uppercase tracking-wider">
          <span className="[&_svg]:size-4 text-primary">{icon}</span>
          {title}
        </CardDescription>
        <CardTitle className="font-heading text-3xl font-bold tabular-nums">
          {loading ? <Skeleton className="h-8 w-20" /> : value}
        </CardTitle>
        <CardDescription className="text-xs">
          {loading ? <Skeleton className="h-3.5 w-32" /> : sub}
        </CardDescription>
      </CardHeader>
    </Card>
  )
}

export function StatsCards({ stats }: { stats: ServerStats | null }) {
  const { t } = useI18n()
  const s = stats
  const loading = s === null
  const total = s ? s.successful_requests + s.failed_requests : 0
  return (
    <div className="grid grid-cols-1 gap-4 sm:grid-cols-2 xl:grid-cols-3">
      <StatCard
        loading={loading}
        icon={<GlobeIcon />}
        title={t('Tunnel Clients')}
        value={String(s?.connected_clients_count ?? 0)}
        sub={
          s && s.connected_clients_count > 0
            ? t('{count} tunnel client(s) active', { count: s.connected_clients_count })
            : t('No active web socket client')
        }
      />
      <StatCard
        loading={loading}
        icon={<LayersIcon />}
        title={t('Queue Status')}
        value={String(s?.pending_requests_count ?? 0)}
        sub={t('Requests pending reconnection')}
      />
      <StatCard
        loading={loading}
        icon={<TrendingUpIcon />}
        title={t('Total Requests')}
        value={String(s?.total_requests ?? 0)}
        sub={t('{ok} of {total} successful', { ok: s?.successful_requests ?? 0, total })}
      />
      <StatCard
        loading={loading}
        icon={<ArrowLeftRightIcon />}
        title={t('Data Transferred')}
        value={formatBytes(s?.total_bytes_transferred ?? 0)}
        sub={t('Payload bytes transferred')}
      />
      <StatCard
        loading={loading}
        icon={<GaugeIcon />}
        title={t('Avg Response')}
        value={s && s.persistent.total_requests > 0 ? `${s.avg_response_ms.toFixed(1)} ms` : '—'}
        sub={
          s
            ? t('{count} lifetime requests • {bytes} sent', { count: s.persistent.total_requests, bytes: formatBytes(s.persistent.total_bytes_sent) })
            : t('All-time (persisted)')
        }
      />
      <StatCard
        loading={loading}
        icon={<CalendarDaysIcon />}
        title={t('Today')}
        value={String(s?.today.requests ?? 0)}
        sub={
          s
            ? t('{ok} ok / {failed} failed • {bytes} sent today', { ok: s.today.success, failed: s.today.failed, bytes: formatBytes(s.today.bytes_sent) })
            : t('Requests today')
        }
      />
    </div>
  )
}
