import { ClockIcon, GlobeIcon, InboxIcon, RefreshCwIcon, SendIcon, Trash2Icon } from 'lucide-react'
import { useCallback, useEffect, useState } from 'react'
import { toast } from 'sonner'
import {
  RecordEmpty,
  RecordFact,
  RecordList,
  RecordRow,
  RecordSkeleton,
  SectionHeader,
} from './shared'
import { MethodBadge, StatusBadge } from './badges'
import { Button } from '@/components/ui/button'
import { formatAbsoluteTime, formatRelativeTime } from '@/lib/format'
import { useI18n } from '@/i18n'
import { api, ApiError, type InboxDetail, type InboxSummary } from '@/lib/api'

function decodeBody(b64: string | null, t: (key: string) => string): string {
  if (!b64) return ''
  try {
    const text = atob(b64)
    try {
      return JSON.stringify(JSON.parse(text), null, 2)
    } catch {
      return text
    }
  } catch {
    return t('(binary payload)')
  }
}

/**
 * Webhook inbox: inbound third-party webhooks persisted for services that
 * opted in with `webhook_inbox: true`. Each entry expands into its headers
 * and payload and can be re-fired to the currently connected client, the
 * cure for "Stripe fired while my laptop was closed".
 */
export function InboxSection() {
  const { t } = useI18n()
  const [entries, setEntries] = useState<InboxSummary[] | null>(null)
  const [openId, setOpenId] = useState<string | null>(null)
  const [detail, setDetail] = useState<InboxDetail | null>(null)
  const [busy, setBusy] = useState<string | null>(null)

  const reload = useCallback(() => {
    api
      .inbox()
      .then((rows) => setEntries(rows))
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
    // Drop a reply that arrives after `openId` moved on, so a slow fetch for
    // the previously opened entry cannot overwrite the one now on screen.
    let cancelled = false
    api
      .inboxEntry(openId)
      .then((d) => {
        if (!cancelled) setDetail(d)
      })
      .catch(() => {
        if (!cancelled) setDetail(null)
      })
    return () => {
      cancelled = true
    }
  }, [openId])

  const refire = async (id: string) => {
    setBusy(id)
    try {
      const body = await api.inboxRefire(id)
      toast.success(t('Re-fired, backend answered {status}', { status: body.status }))
    } catch (e) {
      toast.error(
        t('Re-fire failed ({status})', { status: e instanceof ApiError ? e.status : 0 }),
      )
    } finally {
      setBusy(null)
    }
  }

  // A delete that fails quietly is followed by a reload showing the entry
  // still there, which reads as the button not working rather than as the
  // request being refused.
  const report = (e: unknown) =>
    toast.error(t('Could not delete ({status})', { status: e instanceof ApiError ? e.status : 0 }))

  const remove = async (id: string) => {
    await api.inboxDelete(id).catch(report)
    if (openId === id) setOpenId(null)
    reload()
  }

  const clearAll = async () => {
    await api.inboxClear().catch(report)
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
          'Services with webhook_inbox: true get every inbound POST persisted here, browse the payloads and re-fire any event to the connected client.',
        )}
      </p>

      <RecordList>
        {entries === null ? (
          <RecordSkeleton rows={4} />
        ) : entries.length === 0 ? (
          <RecordEmpty icon={<InboxIcon />}>
            {t('No captured webhooks yet, opt a service in with webhook_inbox: true.')}
          </RecordEmpty>
        ) : (
          entries.map((e) => (
            <div
              key={e.id}
              className="cursor-pointer hover:bg-muted/40"
              onClick={() => setOpenId((cur) => (cur === e.id ? null : e.id))}
            >
              <RecordRow
                title={
                  <>
                    <MethodBadge method={e.method} />
                    <span className="break-all font-mono">{e.uri}</span>
                    <StatusBadge status={e.status} />
                  </>
                }
                actions={
                  <div className="flex gap-1.5" onClick={(ev) => ev.stopPropagation()}>
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
                }
              >
                <RecordFact icon={<ClockIcon />} title={formatAbsoluteTime(e.timestamp)}>
                  {formatRelativeTime(e.timestamp, t)}
                </RecordFact>
                <RecordFact icon={<GlobeIcon />} className="font-mono">
                  {e.host ?? '-'}
                </RecordFact>
                <RecordFact className="tabular-nums">
                  {e.body_bytes} B{e.body_truncated ? ` ${t('(truncated)')}` : ''}
                </RecordFact>
              </RecordRow>
            </div>
          ))
        )}
      </RecordList>

      {openId && detail && (
        <div className="flex flex-col gap-3 rounded-3xl border p-5">
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
              {decodeBody(detail.body, t) || t('(no body)')}
            </pre>
          </div>
        </div>
      )}
    </section>
  )
}
