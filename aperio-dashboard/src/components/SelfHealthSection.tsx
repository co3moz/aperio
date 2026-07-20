import { DownloadIcon, HeartPulseIcon } from 'lucide-react'
import { SectionHeader } from './shared'
import { Button } from '@/components/ui/button'
import { Card } from '@/components/ui/card'
import { usePoll } from '@/hooks/usePoll'
import { api, type SelfHealth } from '@/lib/api'
import { formatBytes, formatUptime } from '@/lib/format'
import { useI18n } from '@/i18n'
import { useHasRole } from '@/lib/session'

// Server self-health (process memory, store size, cache) plus a CSV export of
// the traffic history. Master-admin only for the self-health figures.
export function SelfHealthSection() {
  const { t } = useI18n()
  const isAdmin = useHasRole('admin')
  const { data } = usePoll<SelfHealth>(() => api.selfHealth(), 15000)

  const tiles: [string, string][] = data
    ? [
        [t('Uptime'), formatUptime(data.uptime_seconds)],
        [t('Clients'), String(data.connected_clients)],
        [t('Memory (RSS)'), data.rss_bytes == null ? '—' : formatBytes(data.rss_bytes)],
        [t('Store size'), formatBytes(data.store_bytes)],
      ]
    : []

  return (
    <Card className="p-4">
      <SectionHeader
        title={t('Server self-health')}
        description={t('Process memory, store size, and traffic export.')}
      >
        <Button size="sm" variant="outline" render={<a href="/aperio/api/export/traffic.csv?unit=day&count=90" />}>
          <DownloadIcon /> {t('Export traffic CSV')}
        </Button>
      </SectionHeader>
      {!data ? (
        <p className="text-sm text-muted-foreground">
          <HeartPulseIcon className="mr-1 inline size-4" />
          {isAdmin ? t('No self-health data yet') : t('Requires the master admin')}
        </p>
      ) : (
        <div className="grid grid-cols-2 gap-3 sm:grid-cols-4">
          {tiles.map(([label, value]) => (
            <div key={label} className="rounded-md border p-3">
              <div className="text-xs text-muted-foreground">{label}</div>
              <div className="text-lg font-semibold tabular-nums">{value}</div>
            </div>
          ))}
        </div>
      )}
    </Card>
  )
}
