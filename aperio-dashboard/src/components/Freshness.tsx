import { RefreshCwIcon } from 'lucide-react'
import { useEffect, useState } from 'react'
import { Button } from '@/components/ui/button'
import { streamHub } from '@/lib/stream'
import { useI18n } from '@/i18n'

/**
 * How current a section is (planned_features #164).
 *
 * A page whose document arrives over the stream does not need a Refresh
 * button; it needs to say when it last heard, so a reader can tell a live
 * table from one the stream stopped feeding. While the stream is up this
 * is that stamp. When it is down, or the section is a poll rather than a
 * topic, the button comes back, since then the reader is the only thing
 * that can ask again.
 */
export function Freshness({
  updatedAt,
  onRefresh,
  live,
}: {
  /** When the section last received its document, or null before the first. */
  updatedAt: number | null
  onRefresh: () => void
  /** Whether the section is fed by the stream; unset means "while the stream is up". */
  live?: boolean
}) {
  const { t } = useI18n()
  const [streamUp, setStreamUp] = useState(streamHub.isUp())
  useEffect(() => streamHub.onStatus(setStreamUp), [])
  // Re-render on a slow tick so the stamp ages on screen.
  const [, setTick] = useState(0)
  useEffect(() => {
    const timer = setInterval(() => setTick((n) => n + 1), 5_000)
    return () => clearInterval(timer)
  }, [])
  const isLive = live ?? streamUp
  if (!isLive) {
    return (
      <Button size="sm" variant="outline" onClick={onRefresh}>
        <RefreshCwIcon /> {t('Refresh')}
      </Button>
    )
  }
  const ago = updatedAt === null ? null : Math.max(0, Math.round((Date.now() - updatedAt) / 1000))
  return (
    <span className="text-xs text-muted-foreground" title={t('Pushed over the live stream; this is when it last changed.')}>
      {ago === null ? t('waiting for data…') : ago < 5 ? t('updated just now') : t('updated {seconds}s ago', { seconds: ago })}
    </span>
  )
}
