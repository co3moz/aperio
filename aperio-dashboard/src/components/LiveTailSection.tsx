import { ArrowDownIcon, PauseIcon, PlayIcon, SearchIcon, Trash2Icon } from 'lucide-react'
import { useEffect, useRef, useState } from 'react'
import { SectionHeader } from './shared'
import { Button } from '@/components/ui/button'
import { Card } from '@/components/ui/card'
import { Input } from '@/components/ui/input'
import { Tooltip, TooltipContent, TooltipTrigger } from '@/components/ui/tooltip'
import type { RequestLog } from '@/lib/api'
import { cn } from '@/lib/utils'
import { useI18n } from '@/i18n'

// Rendered line cap — the tail keeps only the newest window, like a terminal
// scrollback, so a busy tunnel cannot grow the DOM without bound.
const MAX_LINES = 500

function statusClass(log: RequestLog): string {
  if (log.error || log.status == null || log.status >= 500) return 'text-red-500'
  if (log.status >= 400) return 'text-amber-500'
  if (log.status >= 300) return 'text-sky-500'
  return 'text-emerald-500'
}

function lineTime(timestamp: string): string {
  const d = new Date(timestamp)
  return Number.isNaN(d.getTime()) ? timestamp : d.toLocaleTimeString([], { hour12: false })
}

/**
 * `tail -f`-style live view of the access log: one monospace line per proxied
 * request, streamed via the same SSE feed as the traffic table. Auto-scrolls
 * while the view is pinned to the bottom; scrolling up unpins so history can
 * be read, and a jump-to-bottom control re-pins.
 */
export function LiveTailSection({
  logs,
  onInspect,
}: {
  logs: RequestLog[] | null
  onInspect: (id: string) => void
}) {
  const { t } = useI18n()
  const [filter, setFilter] = useState('')
  const [paused, setPaused] = useState(false)
  const [frozen, setFrozen] = useState<RequestLog[]>([])
  // Ids cleared from view (kept so "clear" doesn't refill from the shared feed).
  const [clearedBefore, setClearedBefore] = useState<string | null>(null)
  const [pinned, setPinned] = useState(true)
  const scroller = useRef<HTMLDivElement>(null)

  const togglePause = () => {
    if (!paused) setFrozen(logs ?? [])
    setPaused((p) => !p)
  }

  const source = paused ? frozen : (logs ?? [])
  // "Clear" remembers the newest id at clear time; only later entries show.
  const afterClear = clearedBefore
    ? source.slice(source.findIndex((l) => l.id === clearedBefore) + 1)
    : source
  const needle = filter.toLowerCase()
  const matched = needle
    ? afterClear.filter(
        (l) =>
          l.uri.toLowerCase().includes(needle) ||
          l.method.toLowerCase().includes(needle) ||
          (l.host ?? '').toLowerCase().includes(needle) ||
          String(l.status ?? '').includes(needle),
      )
    : afterClear
  const visible = matched.slice(-MAX_LINES)

  // Follow the stream: keep the newest line in view while pinned.
  useEffect(() => {
    if (pinned && scroller.current) {
      scroller.current.scrollTop = scroller.current.scrollHeight
    }
  }, [visible.length, pinned])

  const onScroll = () => {
    const el = scroller.current
    if (!el) return
    const atBottom = el.scrollHeight - el.scrollTop - el.clientHeight < 24
    setPinned(atBottom)
  }

  return (
    <section className="flex flex-col gap-3">
      <SectionHeader title={t('Live Log Tail')}>
        <Tooltip>
          <TooltipTrigger
            render={
              <Button size="sm" variant={paused ? 'secondary' : 'outline'} onClick={togglePause} />
            }
          >
            {paused ? <PlayIcon /> : <PauseIcon />} {paused ? t('Paused') : t('Live')}
          </TooltipTrigger>
          <TooltipContent>
            {paused ? t('Resume live updates') : t('Freeze the stream while you read')}
          </TooltipContent>
        </Tooltip>
        <Button
          size="sm"
          variant="outline"
          onClick={() => setClearedBefore(source.length ? source[source.length - 1].id : null)}
        >
          <Trash2Icon /> {t('Clear')}
        </Button>
        <div className="relative">
          <SearchIcon className="pointer-events-none absolute left-2.5 top-1/2 size-4 -translate-y-1/2 text-muted-foreground" />
          <Input
            placeholder={t('Filter by host/path/method/status...')}
            value={filter}
            onChange={(e) => setFilter(e.target.value)}
            className="w-72 pl-8"
          />
        </div>
      </SectionHeader>

      <Card className="relative overflow-hidden py-0">
        <div
          ref={scroller}
          onScroll={onScroll}
          className="h-[32rem] overflow-y-auto bg-zinc-950 px-4 py-3 font-mono text-xs leading-6 text-zinc-100"
        >
          {visible.length === 0 ? (
            <div className="flex h-full items-center justify-center text-zinc-500">
              {logs === null ? t('Waiting for the stream...') : t('No requests yet — the tail follows new traffic as it arrives.')}
            </div>
          ) : (
            visible.map((log) => (
              <button
                key={log.id}
                type="button"
                title={t('Click to inspect & replay')}
                onClick={() => onInspect(log.id)}
                className="block w-full cursor-pointer whitespace-pre-wrap break-all text-left hover:bg-zinc-900"
              >
                <span className="text-zinc-500">{lineTime(log.timestamp)}</span>{' '}
                <span className={cn('font-semibold', statusClass(log))}>
                  {log.status ?? 'ERR'}
                </span>{' '}
                <span className="text-zinc-300">{log.method.padEnd(6)}</span>
                <span className="text-violet-400">{log.host ?? '-'}</span>{' '}
                <span className="text-zinc-100">{log.uri}</span>{' '}
                <span className="text-zinc-500">{log.duration_ms}ms</span>
                {log.error ? <span className="text-red-400"> {log.error}</span> : null}
              </button>
            ))
          )}
        </div>
        {!pinned && (
          <Button
            size="sm"
            variant="secondary"
            className="absolute bottom-4 right-6 shadow-lg"
            onClick={() => {
              setPinned(true)
              if (scroller.current) scroller.current.scrollTop = scroller.current.scrollHeight
            }}
          >
            <ArrowDownIcon /> {t('Jump to latest')}
          </Button>
        )}
      </Card>

      <div className="flex flex-wrap justify-between gap-2">
        <span className="text-xs text-muted-foreground">
          {t('Showing the newest {max} lines; click a line to open the inspector.', {
            max: MAX_LINES,
          })}
        </span>
        {paused && (
          <span className="text-xs text-amber-600 dark:text-amber-400">
            {t('Paused — stream frozen at {count} requests.', { count: frozen.length })}
          </span>
        )}
      </div>
    </section>
  )
}
