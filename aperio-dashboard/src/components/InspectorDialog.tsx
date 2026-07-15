import { CheckIcon, CopyIcon, DownloadIcon, PlayIcon } from 'lucide-react'
import { useEffect, useState } from 'react'
import { toast } from 'sonner'
import { PreBlock } from './shared'
import { Button } from '@/components/ui/button'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { Spinner } from '@/components/ui/spinner'
import { Tooltip, TooltipContent, TooltipTrigger } from '@/components/ui/tooltip'
import { api, ApiError, type CapturedRequest, type RequestTimeline } from '@/lib/api'
import { buildCurl, buildHar, decodeBodyPreview, formatHeaders } from '@/lib/format'
import { cn } from '@/lib/utils'
import { useI18n } from '@/i18n'

function Section({ label, content }: { label: string; content: string }) {
  return (
    <div className="flex flex-col gap-1">
      <span className="text-xs font-semibold uppercase tracking-wider text-muted-foreground">
        {label}
      </span>
      <PreBlock>{content}</PreBlock>
    </div>
  )
}

/** Detail view for a captured request, with one-click replay. */

/** One waterfall row: a stage interval as offsets from t0 (µs). */
interface Stage {
  label: string
  from: number
  to: number
  estimated?: boolean
}

function fmtUs(us: number): string {
  if (us >= 1_000_000) return `${(us / 1_000_000).toFixed(2)} s`
  if (us >= 1_000) return `${(us / 1_000).toFixed(2)} ms`
  return `${us} µs`
}

function TimelineWaterfall({ timeline }: { timeline: RequestTimeline }) {
  const { t } = useI18n()
  const tl = timeline
  const total = Math.max(tl.finished_us, 1)
  const stages: Stage[] = []
  stages.push({ label: t('queued & routed'), from: 0, to: tl.dispatched_us })
  if (
    tl.client_received_us !== undefined &&
    tl.backend_sent_us !== undefined &&
    tl.backend_first_byte_us !== undefined &&
    tl.backend_done_us !== undefined &&
    tl.client_responded_us !== undefined
  ) {
    stages.push({ label: t('tunnel → client'), from: tl.dispatched_us, to: tl.client_received_us, estimated: true })
    stages.push({ label: t('client processing'), from: tl.client_received_us, to: tl.backend_sent_us, estimated: true })
    stages.push({ label: t('backend wait (first byte)'), from: tl.backend_sent_us, to: tl.backend_first_byte_us, estimated: true })
    stages.push({ label: t('backend body'), from: tl.backend_first_byte_us, to: tl.backend_done_us, estimated: true })
    stages.push({ label: t('client → tunnel'), from: tl.backend_done_us, to: tl.client_responded_us, estimated: true })
    stages.push({ label: t('tunnel → server'), from: tl.client_responded_us, to: tl.response_received_us, estimated: true })
  } else {
    stages.push({ label: t('tunnel round-trip (client & backend)'), from: tl.dispatched_us, to: tl.response_received_us })
  }
  stages.push({ label: t('server → visitor'), from: tl.response_received_us, to: tl.finished_us })

  return (
    <div className="flex flex-col gap-1.5">
      <h4 className="text-xs font-semibold uppercase tracking-wide text-muted-foreground">
        {t('Timeline')}
        <span className="ml-2 font-normal normal-case">
          {t('offsets from arrival at the server')}
          {tl.estimated_anchor ? ` · ${t('client stages anchored by splitting transit evenly')}` : ''}
        </span>
      </h4>
      <div className="rounded-xl border p-3">
        {stages.map((st) => {
          const width = Math.max(((st.to - st.from) / total) * 100, 0.5)
          const left = (st.from / total) * 100
          return (
            <div key={st.label} className="mb-1.5 last:mb-0">
              <div className="flex items-baseline justify-between gap-2 text-[11px]">
                <span className="text-muted-foreground">
                  {st.label}
                  {st.estimated ? '*' : ''}
                </span>
                <span className="whitespace-nowrap font-mono tabular-nums text-muted-foreground">
                  +{fmtUs(st.from)} → +{fmtUs(st.to)} ({fmtUs(Math.max(st.to - st.from, 0))})
                </span>
              </div>
              <div className="h-2 w-full rounded bg-muted">
                <div
                  className={st.estimated ? 'h-2 rounded bg-primary/50' : 'h-2 rounded bg-primary'}
                  style={{ marginLeft: `${left}%`, width: `${width}%` }}
                />
              </div>
            </div>
          )
        })}
      </div>
    </div>
  )
}

export function InspectorDialog({ id, onClose }: { id: string | null; onClose: () => void }) {
  const { t } = useI18n()
  const [detail, setDetail] = useState<CapturedRequest | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [replayResult, setReplayResult] = useState<string | null>(null)
  const [replaying, setReplaying] = useState(false)
  const [copied, setCopied] = useState(false)

  useEffect(() => {
    setDetail(null)
    setError(null)
    setReplayResult(null)
    setCopied(false)
    if (!id) return
    api
      .requestDetail(id)
      .then(setDetail)
      .catch((e: unknown) => {
        setError(
          e instanceof ApiError && e.status === 404
            ? t('Detail not available for this request (only recent requests are captured).')
            : t('Failed to load request detail: {error}', {
                error: e instanceof Error ? e.message : String(e),
              }),
        )
      })
  }, [id])

  const copyCurl = async () => {
    if (!detail) return
    const curl = buildCurl(
      detail.method,
      detail.uri,
      detail.req_headers,
      detail.req_body,
      detail.req_body_truncated,
    )
    try {
      await navigator.clipboard.writeText(curl)
      setCopied(true)
      toast.success(t('Copied cURL to clipboard'))
    } catch {
      toast.error(t('Clipboard unavailable'))
    }
  }

  const exportHar = () => {
    if (!detail) return
    const blob = new Blob([buildHar(detail)], { type: 'application/json' })
    const url = URL.createObjectURL(blob)
    const a = document.createElement('a')
    a.href = url
    a.download = `request-${detail.id}.har`
    a.click()
    URL.revokeObjectURL(url)
  }

  const replay = async () => {
    if (!detail) return
    setReplaying(true)
    setReplayResult(null)
    try {
      const r = await api.replayRequest(detail.id)
      setReplayResult(t('✔ Replayed: status {status} in {ms} ms', { status: r.status, ms: r.duration_ms }))
    } catch (e) {
      setReplayResult(t('Replay failed: {error}', { error: e instanceof Error ? e.message : String(e) }))
    } finally {
      setReplaying(false)
    }
  }

  return (
    <Dialog
      open={id !== null}
      onOpenChange={(open) => {
        if (!open) onClose()
      }}
    >
      <DialogContent className="max-h-[85vh] overflow-y-auto sm:max-w-3xl">
        <DialogHeader>
          <div className="flex items-start justify-between gap-3 pr-8">
            <DialogTitle className="break-all leading-snug">
              {detail
                ? `${detail.method} ${detail.uri} → ${detail.status} (${detail.duration_ms} ms)`
                : t('Request Detail')}
            </DialogTitle>
            {detail && (
              <div className="flex shrink-0 items-center gap-2">
                <Tooltip>
                  <TooltipTrigger render={<Button size="xs" variant="outline" onClick={copyCurl} />}>
                    {copied ? <CheckIcon /> : <CopyIcon />} {copied ? t('Copied') : t('Copy as cURL')}
                  </TooltipTrigger>
                  <TooltipContent>{t('Copy an equivalent curl command')}</TooltipContent>
                </Tooltip>
                <Tooltip>
                  <TooltipTrigger render={<Button size="xs" variant="outline" onClick={exportHar} />}>
                    <DownloadIcon /> {t('HAR')}
                  </TooltipTrigger>
                  <TooltipContent>{t('Download as an HAR file (devtools importable)')}</TooltipContent>
                </Tooltip>
                <Tooltip>
                  <TooltipTrigger
                    render={
                      <Button
                        size="xs"
                        variant="outline"
                        disabled={detail.req_body_truncated || replaying}
                        onClick={replay}
                      />
                    }
                  >
                    {replaying ? <Spinner /> : <PlayIcon />} {t('Replay')}
                  </TooltipTrigger>
                  <TooltipContent>
                    {detail.req_body_truncated
                      ? t('Body truncated at capture; cannot replay')
                      : t('Send this request through the tunnel again')}
                  </TooltipContent>
                </Tooltip>
              </div>
            )}
          </div>
          <DialogDescription>
            {t('Captured transaction detail — bodies are capped at 64 KB.')}
          </DialogDescription>
        </DialogHeader>
        <div className="flex flex-col gap-4">
          {replayResult && (
            <p
              className={cn(
                'rounded-2xl border px-3 py-2 text-sm',
                replayResult.startsWith('✔')
                  ? 'border-emerald-500/30 bg-emerald-500/10 text-emerald-700 dark:text-emerald-400'
                  : 'border-red-500/30 bg-red-500/10 text-red-700 dark:text-red-400',
              )}
            >
              {replayResult}
            </p>
          )}
          {error && <p className="text-sm text-destructive">{error}</p>}
          {detail && (
            <>
              {detail.timeline && <TimelineWaterfall timeline={detail.timeline} />}
              <Section label={t('Request Headers')} content={formatHeaders(detail.req_headers)} />
              <Section
                label={t('Request Body')}
                content={decodeBodyPreview(detail.req_body, detail.req_body_truncated, false)}
              />
              <Section label={t('Response Headers')} content={formatHeaders(detail.resp_headers)} />
              <Section
                label={t('Response Body')}
                content={decodeBodyPreview(
                  detail.resp_body,
                  detail.resp_body_truncated,
                  detail.resp_streamed,
                )}
              />
            </>
          )}
        </div>
        <DialogFooter>
          <Button variant="outline" onClick={onClose}>
            {t('Close')}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
