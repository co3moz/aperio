import { InboxIcon, RefreshCwIcon, SendIcon, Trash2Icon } from 'lucide-react'
import { useCallback, useEffect, useState } from 'react'
import { toast } from 'sonner'
import { EmptyRow, SectionHeader, SkeletonRows } from './shared'
import { MethodBadge, StatusBadge } from './badges'
import { Button } from '@/components/ui/button'
import { Card } from '@/components/ui/card'
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table'
import { formatAbsoluteTime, formatRelativeTime } from '@/lib/format'
import { useI18n } from '@/i18n'

interface InboxSummary {
  id: string
  timestamp: string
  method: string
  uri: string
  host: string | null
  status: number
  service: string | null
  body_bytes: number
  body_truncated: boolean
}

interface InboxDetail extends InboxSummary {
  headers: [string, string][]
  body: string | null
}

function decodeBody(b64: string | null): string {
  if (!b64) return ''
  try {
    const text = atob(b64)
    try {
      return JSON.stringify(JSON.parse(text), null, 2)
    } catch {
      return text
    }
  } catch {
    return '(binary payload)'
  }
}

/**
 * Webhook inbox: inbound third-party webhooks persisted for services that
 * opted in with `webhook_inbox: true`. Each entry expands into its headers
 * and payload and can be re-fired to the currently connected client — the
 * cure for "Stripe fired while my laptop was closed".
 */
export function InboxSection() {
  const { t } = useI18n()
  const [entries, setEntries] = useState<InboxSummary[] | null>(null)
  const [openId, setOpenId] = useState<string | null>(null)
  const [detail, setDetail] = useState<InboxDetail | null>(null)
  const [busy, setBusy] = useState<string | null>(null)

  const reload = useCallback(() => {
    fetch('/aperio/api/inbox')
      .then((r) => r.json())
      .then((rows: InboxSummary[]) => setEntries(rows))
      .catch(() => setEntries([]))
  }, [])

  useEffect(() => {
    reload()
  }, [reload])

  useEffect(() => {
    if (!openId) {
      setDetail(null)
      return
    }
    fetch(`/aperio/api/inbox/${openId}`)
      .then((r) => (r.ok ? r.json() : null))
      .then((d: InboxDetail | null) => setDetail(d))
      .catch(() => setDetail(null))
  }, [openId])

  const refire = async (id: string) => {
    setBusy(id)
    try {
      const res = await fetch(`/aperio/api/inbox/${id}/refire`, { method: 'POST' })
      const body = await res.json().catch(() => null)
      if (res.ok && body) {
        toast.success(t('Re-fired — backend answered {status}', { status: body.status }))
      } else {
        toast.error(t('Re-fire failed ({status})', { status: res.status }))
      }
    } catch {
      toast.error(t('Re-fire failed ({status})', { status: 0 }))
    } finally {
      setBusy(null)
    }
  }

  const remove = async (id: string) => {
    await fetch(`/aperio/api/inbox/${id}`, { method: 'DELETE' }).catch(() => {})
    if (openId === id) setOpenId(null)
    reload()
  }

  const clearAll = async () => {
    await fetch('/aperio/api/inbox', { method: 'DELETE' }).catch(() => {})
    setOpenId(null)
    reload()
  }

  return (
    <section className="flex flex-col gap-3">
      <SectionHeader title={t('Webhook Inbox')}>
        <Button size="sm" variant="outline" onClick={reload}>
          <RefreshCwIcon /> {t('Refresh')}
        </Button>
        <Button
          size="sm"
          variant="outline"
          onClick={() => void clearAll()}
          disabled={!entries || entries.length === 0}
        >
          <Trash2Icon /> {t('Clear inbox')}
        </Button>
      </SectionHeader>

      <p className="max-w-3xl text-sm text-muted-foreground">
        {t(
          'Services with webhook_inbox: true get every inbound POST persisted here — browse the payloads and re-fire any event to the connected client.',
        )}
      </p>

      <Card className="overflow-hidden py-0">
        <Table>
          <TableHeader>
            <TableRow>
              <TableHead>{t('Timestamp')}</TableHead>
              <TableHead>{t('Method')}</TableHead>
              <TableHead>{t('Hostname')}</TableHead>
              <TableHead>{t('Path')}</TableHead>
              <TableHead>{t('Status')}</TableHead>
              <TableHead>{t('Payload')}</TableHead>
              <TableHead className="text-right">{t('Actions')}</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {entries === null ? (
              <SkeletonRows rows={4} cols={7} />
            ) : entries.length === 0 ? (
              <EmptyRow colSpan={7} icon={<InboxIcon />}>
                {t('No captured webhooks yet — opt a service in with webhook_inbox: true.')}
              </EmptyRow>
            ) : (
              entries.map((e) => (
                <TableRow
                  key={e.id}
                  className="cursor-pointer"
                  onClick={() => setOpenId((cur) => (cur === e.id ? null : e.id))}
                >
                  <TableCell>
                    <span className="font-mono text-xs text-muted-foreground" title={formatAbsoluteTime(e.timestamp)}>
                      {formatRelativeTime(e.timestamp)}
                    </span>
                  </TableCell>
                  <TableCell>
                    <MethodBadge method={e.method} />
                  </TableCell>
                  <TableCell className="font-mono text-xs">{e.host ?? '-'}</TableCell>
                  <TableCell>
                    <span className="inline-block max-w-80 break-all font-mono text-sm">{e.uri}</span>
                  </TableCell>
                  <TableCell>
                    <StatusBadge status={e.status} />
                  </TableCell>
                  <TableCell className="font-mono text-xs tabular-nums">
                    {e.body_bytes} B{e.body_truncated ? ' (truncated)' : ''}
                  </TableCell>
                  <TableCell className="text-right">
                    <div className="flex justify-end gap-1.5" onClick={(ev) => ev.stopPropagation()}>
                      <Button
                        size="xs"
                        variant="outline"
                        disabled={busy === e.id || e.body_truncated}
                        onClick={() => void refire(e.id)}
                        title={t('Re-fire to the connected client')}
                      >
                        <SendIcon /> {t('Re-fire')}
                      </Button>
                      <Button size="xs" variant="ghost" onClick={() => void remove(e.id)}>
                        <Trash2Icon />
                      </Button>
                    </div>
                  </TableCell>
                </TableRow>
              ))
            )}
          </TableBody>
        </Table>
      </Card>

      {openId && detail && (
        <Card className="gap-3 p-5">
          <h3 className="text-sm font-semibold">
            {detail.method} {detail.uri}
          </h3>
          <div>
            <h4 className="mb-1 text-xs font-semibold uppercase tracking-wider text-muted-foreground">
              {t('Headers')}
            </h4>
            <pre className="max-h-48 overflow-auto rounded-md bg-muted p-3 font-mono text-xs">
              {detail.headers.map(([k, v]) => `${k}: ${v}`).join('\n')}
            </pre>
          </div>
          <div>
            <h4 className="mb-1 text-xs font-semibold uppercase tracking-wider text-muted-foreground">
              {t('Payload')}
            </h4>
            <pre className="max-h-80 overflow-auto rounded-md bg-muted p-3 font-mono text-xs">
              {decodeBody(detail.body) || t('(no body)')}
            </pre>
          </div>
        </Card>
      )}
    </section>
  )
}
