import { DatabaseIcon, Trash2Icon } from 'lucide-react'
import { toast } from 'sonner'
import { SectionHeader } from './shared'
import { Button } from '@/components/ui/button'
import { Card } from '@/components/ui/card'
import { usePoll } from '@/hooks/usePoll'
import { api, type CacheStats } from '@/lib/api'
import { useI18n } from '@/i18n'
import { useHasRole } from '@/lib/session'

function bytes(n: number): string {
  if (n < 1024) return `${n} B`
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`
  return `${(n / (1024 * 1024)).toFixed(1)} MB`
}

// Response-cache occupancy and hit rate, with a purge-all control.
export function CacheStatsSection() {
  const { t } = useI18n()
  const isAdmin = useHasRole('admin')
  const { data, refresh } = usePoll<CacheStats>(() => api.cacheStats(), 15000)

  const purgeAll = async () => {
    try {
      const res = await api.purgeCache({})
      refresh()
      toast.success(t('Purged {n} cache entries', { n: res.removed }))
    } catch (e) {
      toast.error(e instanceof Error ? e.message : String(e))
    }
  }

  const tiles: [string, string][] = data
    ? [
        [t('Entries'), String(data.entries)],
        [t('Size'), bytes(data.bytes)],
        [t('Hit ratio'), `${(data.hit_ratio * 100).toFixed(1)}%`],
        [t('Hits / misses'), `${data.hits} / ${data.misses}`],
      ]
    : []

  return (
    <Card className="p-4">
      <SectionHeader
        title={t('Response cache')}
        description={t('Server-side GET cache occupancy and hit rate.')}
      >
        {isAdmin && (
          <Button size="sm" variant="outline" onClick={purgeAll}>
            <Trash2Icon /> {t('Purge all')}
          </Button>
        )}
      </SectionHeader>
      {!data ? (
        <p className="text-sm text-muted-foreground">
          <DatabaseIcon className="mr-1 inline size-4" />
          {t('No cache data yet')}
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
