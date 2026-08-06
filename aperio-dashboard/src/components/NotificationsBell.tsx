import { BellIcon } from 'lucide-react'
import { useMemo, useState } from 'react'
import { Button } from '@/components/ui/button'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import { useI18n } from '@/i18n'
import { formatRelativeTime } from '@/lib/format'
import { detailOf, isUrgent, severityOf, type ServerNotification } from '@/lib/notifications'
import { cn } from '@/lib/utils'

// Which notifications have been read survives a reload: a bell that came back
// with everything unread after every refresh would be a counter nobody trusts.
const SEEN_KEY = 'aperio-notifications-seen'

function loadSeen(): number {
  try {
    const raw = localStorage.getItem(SEEN_KEY)
    const value = raw ? Number(raw) : 0
    return Number.isFinite(value) ? value : 0
  } catch {
    // Unavailable storage: everything is unread, which is the safe direction.
    return 0
  }
}

const DOT: Record<ReturnType<typeof severityOf>, string> = {
  good: 'bg-emerald-500',
  bad: 'bg-red-500',
  warn: 'bg-amber-500',
  info: 'bg-muted-foreground/50',
}

/**
 * The notification bell: server events as they happen, with unread state.
 *
 * Everything here already existed as an event, and until now the only way to
 * find out that a client had dropped or a token was about to expire was to
 * notice it in a table. The events arrive on the dashboard's existing SSE
 * stream, already fenced to the caller's organization by the server, so this
 * adds a view rather than a data path.
 *
 * Read state is a timestamp rather than a set of ids: the ids are minted per
 * session and mean nothing after a reload, while "everything up to here has
 * been seen" survives one and needs no bookkeeping per item.
 */
export function NotificationsBell({ notifications }: { notifications: ServerNotification[] }) {
  const { t } = useI18n()
  const [seenAt, setSeenAt] = useState<number>(loadSeen)
  const [open, setOpen] = useState(false)

  const unread = useMemo(
    () => notifications.filter((n) => new Date(n.timestamp).getTime() > seenAt),
    [notifications, seenAt],
  )
  const urgent = unread.some((n) => isUrgent(n.event))

  // Closing the panel is what marks things read, not opening it, so the rows
  // that were new stay highlighted for as long as they are being read.
  const onOpenChange = (next: boolean) => {
    setOpen(next)
    if (next) return
    const at = Date.now()
    setSeenAt(at)
    try {
      localStorage.setItem(SEEN_KEY, String(at))
    } catch {
      // Best-effort; the count simply reappears on the next reload.
    }
  }

  // Newest first: a bell is read from the top.
  const rows = useMemo(() => [...notifications].reverse(), [notifications])

  return (
    <DropdownMenu open={open} onOpenChange={onOpenChange}>
      <DropdownMenuTrigger
        render={
          <Button
            variant="ghost"
            size="icon-sm"
            className="relative"
            aria-label={
              unread.length > 0
                ? t('Notifications, {count} unread', { count: unread.length })
                : t('Notifications')
            }
          />
        }
      >
        <BellIcon />
        {unread.length > 0 && (
          <span
            className={cn(
              'absolute -right-0.5 -top-0.5 flex min-w-4 items-center justify-center rounded-full px-1 text-[10px] font-semibold leading-4 text-white',
              urgent ? 'bg-red-500' : 'bg-primary',
            )}
          >
            {unread.length > 9 ? '9+' : unread.length}
          </span>
        )}
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end" className="w-80 max-w-[calc(100vw-2rem)] p-0">
        <div className="flex items-center justify-between border-b px-3 py-2">
          <span className="text-sm font-semibold">{t('Notifications')}</span>
          {unread.length > 0 && (
            <span className="text-xs text-muted-foreground">
              {t('{count} new', { count: unread.length })}
            </span>
          )}
        </div>
        {rows.length === 0 ? (
          <p className="px-3 py-6 text-center text-xs text-muted-foreground">
            {t('Nothing yet. Events show up here as they happen.')}
          </p>
        ) : (
          <ul className="max-h-96 overflow-y-auto">
            {rows.map((n) => {
              const detail = detailOf(n.data)
              return (
                <li
                  key={n.id}
                  className={cn(
                    'flex gap-2 border-b px-3 py-2 last:border-b-0',
                    new Date(n.timestamp).getTime() > seenAt && 'bg-muted/50',
                  )}
                >
                  <span
                    aria-hidden
                    className={cn('mt-1.5 size-2 shrink-0 rounded-full', DOT[severityOf(n.event)])}
                  />
                  <div className="min-w-0 flex-1">
                    <div className="flex items-baseline justify-between gap-2">
                      {/* The event name is an identifier, not prose: it is what
                          a webhook subscribes to and the audit log filters by,
                          so it reads the same in every language. */}
                      <span className="truncate font-mono text-xs">{n.event}</span>
                      <span className="shrink-0 text-[10px] text-muted-foreground">
                        {formatRelativeTime(n.timestamp, t)}
                      </span>
                    </div>
                    {detail && (
                      <p className="truncate text-xs text-muted-foreground" title={detail}>
                        {detail}
                      </p>
                    )}
                  </div>
                </li>
              )
            })}
          </ul>
        )}
      </DropdownMenuContent>
    </DropdownMenu>
  )
}
