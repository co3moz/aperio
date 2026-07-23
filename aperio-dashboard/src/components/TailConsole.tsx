import { ArrowDownIcon, Trash2Icon } from 'lucide-react'
import { useEffect, useRef, useState } from 'react'
import { Button } from '@/components/ui/button'
import { Card } from '@/components/ui/card'
import type { RequestLog } from '@/lib/api'
import { cn } from '@/lib/utils'
import { useI18n } from '@/i18n'

// Rendered line cap — the console keeps only the newest window, like a terminal
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
 * `tail -f`-style console view of the access log: one monospace line per proxied
 * request. It renders the already-filtered, already-pause-gated log window that
 * the traffic view owns, so the search box, method/status chips and pause
 * control are shared with the table view. Auto-scrolls while pinned to the
 * bottom; scrolling up unpins so history can be read, and a jump-to-bottom
 * control re-pins.
 */
export function TailConsole({
  logs,
  loading,
  onInspect,
}: {
  logs: RequestLog[]
  loading: boolean
  onInspect: (id: string) => void
}) {
  const { t } = useI18n()
  // Ids cleared from view (kept so "clear" doesn't refill from the shared feed).
  const [clearedBefore, setClearedBefore] = useState<string | null>(null)
  const [pinned, setPinned] = useState(true)
  const scroller = useRef<HTMLDivElement>(null)

  // "Clear" remembers the newest id at clear time; only later entries show.
  const afterClear = clearedBefore
    ? logs.slice(logs.findIndex((l) => l.id === clearedBefore) + 1)
    : logs
  const visible = afterClear.slice(-MAX_LINES)

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
    <>
      <Card className="relative overflow-hidden py-0">
        <Button
          size="xs"
          variant="secondary"
          className="absolute right-3 top-3 z-10 opacity-80 hover:opacity-100"
          onClick={() => setClearedBefore(logs.length ? logs[logs.length - 1].id : null)}
        >
          <Trash2Icon /> {t('Clear')}
        </Button>
        <div
          ref={scroller}
          onScroll={onScroll}
          className="h-[32rem] overflow-y-auto bg-zinc-950 px-4 py-3 font-mono text-xs leading-6 text-zinc-100"
        >
          {visible.length === 0 ? (
            <div className="flex h-full items-center justify-center text-zinc-500">
              {loading ? t('Waiting for the stream...') : t('No requests matching filter')}
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

      <span className="text-xs text-muted-foreground">
        {t('Showing the newest {max} lines; click a line to open the inspector.', {
          max: MAX_LINES,
        })}
      </span>
    </>
  )
}
