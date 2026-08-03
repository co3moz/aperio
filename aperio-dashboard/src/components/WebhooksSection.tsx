import { ClockIcon, PlusIcon, RotateCwIcon, SendIcon, Trash2Icon } from 'lucide-react'
import { useState } from 'react'
import { toast } from 'sonner'
import {
  RecordEmpty,
  RecordFact,
  RecordList,
  RecordRow,
  RecordSkeleton,
  SectionHeader,
} from './shared'
import { TintBadge } from './badges'
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
  AlertDialogTrigger,
} from '@/components/ui/alert-dialog'
import { Button } from '@/components/ui/button'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from '@/components/ui/dialog'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { Spinner } from '@/components/ui/spinner'
import { usePoll } from '@/hooks/usePoll'
import { api, ApiError, type Webhook, type WebhookDelivery } from '@/lib/api'
import { formatRelativeTime, splitList } from '@/lib/format'
import { useI18n } from '@/i18n'
import { useHasRole } from '@/lib/session'

const KNOWN_EVENTS =
  'client_connected, client_disconnected, client_draining, token_created, token_revoked, token_expiring, tunnel_created, tunnel_deleted, share_created, maintenance_on, maintenance_off, settings_updated, import_applied, alert_triggered, alert_resolved, canary_tripped, token_new_ip, token_pin_mismatch, db_backup, org_usage'

function CreateWebhookDialog({ onCreated }: { onCreated: () => void }) {
  const { t } = useI18n()
  const [open, setOpen] = useState(false)
  const [name, setName] = useState('')
  const [url, setUrl] = useState('')
  const [events, setEvents] = useState('*')
  const [format, setFormat] = useState('generic')
  const [secret, setSecret] = useState('')
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)

  const openDialog = (next: boolean) => {
    if (next) {
      setName('')
      setUrl('')
      setEvents('*')
      setFormat('generic')
      setSecret('')
      setError(null)
    }
    setOpen(next)
  }

  const submit = async () => {
    setBusy(true)
    setError(null)
    try {
      await api.createWebhook({
        name: name.trim(),
        url: url.trim(),
        events: splitList(events),
        format,
        ...(secret.trim() ? { secret: secret.trim() } : {}),
      })
      setOpen(false)
      toast.success(t('Webhook "{name}" added', { name: name.trim() }))
      onCreated()
    } catch (e) {
      setError(e instanceof ApiError ? e.message : String(e))
    } finally {
      setBusy(false)
    }
  }

  return (
    <Dialog open={open} onOpenChange={openDialog}>
      <DialogTrigger render={<Button size="sm" />}>
        <PlusIcon /> {t('Add Webhook')}
      </DialogTrigger>
      <DialogContent className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>{t('Add webhook')}</DialogTitle>
          <DialogDescription>
            {t('Known events: {events}. Use * to subscribe to everything.', { events: KNOWN_EVENTS })}
          </DialogDescription>
        </DialogHeader>
        <div className="grid gap-4">
          <div className="grid gap-2">
            <Label htmlFor="wh-name">{t('Name')}</Label>
            <Input
              id="wh-name"
              value={name}
              onChange={(e) => setName(e.target.value)}
              placeholder="ops-alerts"
            />
          </div>
          <div className="grid gap-2">
            <Label htmlFor="wh-url">{t('URL')}</Label>
            <Input
              id="wh-url"
              type="url"
              value={url}
              onChange={(e) => setUrl(e.target.value)}
              placeholder="https://example.com/hooks/aperio"
              autoComplete="off"
            />
          </div>
          <div className="grid gap-2">
            <Label htmlFor="wh-events">{t('Events (comma separated, * = all)')}</Label>
            <Input
              id="wh-events"
              value={events}
              onChange={(e) => setEvents(e.target.value)}
              placeholder="*"
            />
          </div>
          <div className="grid gap-2">
            <Label htmlFor="wh-format">{t('Payload format')}</Label>
            <Select
              items={{ generic: t('Generic JSON'), slack: 'Slack', discord: 'Discord', teams: 'Microsoft Teams' }}
              value={format}
              onValueChange={(v) => setFormat(v as string)}
            >
              <SelectTrigger id="wh-format" className="w-full">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="generic">{t('Generic JSON')}</SelectItem>
                <SelectItem value="slack">Slack</SelectItem>
                <SelectItem value="discord">Discord</SelectItem>
                <SelectItem value="teams">Microsoft Teams</SelectItem>
              </SelectContent>
            </Select>
            <p className="text-xs text-muted-foreground">
              {t('Generic sends the raw event JSON; the chat formats send a ready-made message for the incoming-webhook URL of that service.')}
            </p>
          </div>
          <div className="grid gap-2">
            <Label htmlFor="wh-secret">{t('Signing secret (optional, 16-128 chars)')}</Label>
            <Input
              id="wh-secret"
              value={secret}
              onChange={(e) => setSecret(e.target.value)}
              placeholder={t('shared secret for X-Aperio-Signature')}
            />
            <p className="text-xs text-muted-foreground">
              {t('Deliveries carry X-Aperio-Signature (HMAC-SHA256 over "timestamp.body") and X-Aperio-Timestamp so the receiver can verify origin and freshness.')}
            </p>
          </div>
          {error && <p className="text-sm text-destructive">{error}</p>}
        </div>
        <DialogFooter>
          <Button variant="outline" onClick={() => setOpen(false)}>
            {t('Cancel')}
          </Button>
          <Button onClick={submit} disabled={busy}>
            {busy && <Spinner />} {t('Add')}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

function TestWebhookButton({ hook, onDone }: { hook: Webhook; onDone: () => void }) {
  const { t } = useI18n()
  const [busy, setBusy] = useState(false)
  const fire = async () => {
    setBusy(true)
    try {
      const r = await api.testWebhook(hook.id)
      if (r.ok) {
        toast.success(
          t('"{name}" answered {status} in {ms} ms', {
            name: hook.name,
            status: String(r.status ?? ''),
            ms: String(r.duration_ms),
          }),
        )
      } else {
        // The reason matters more than the fact: a policy refusal, a timeout
        // and a 500 are three different things to go and fix.
        toast.error(
          t('"{name}" failed: {reason}', {
            name: hook.name,
            reason: r.error ?? (r.status != null ? `HTTP ${r.status}` : t('no response')),
          }),
        )
      }
    } catch (e) {
      toast.error(e instanceof ApiError ? e.message : String(e))
    } finally {
      setBusy(false)
      onDone()
    }
  }

  return (
    <Button size="xs" variant="outline" onClick={fire} disabled={busy}>
      <SendIcon /> {busy ? t('Sending...') : t('Test')}
    </Button>
  )
}

function DeleteWebhookButton({ hook, onDone }: { hook: Webhook; onDone: () => void }) {
  const { t } = useI18n()
  const remove = async () => {
    try {
      await api.deleteWebhook(hook.id)
      toast.info(t('Webhook "{name}" deleted', { name: hook.name }))
    } catch (e) {
      toast.error(e instanceof ApiError ? e.message : String(e))
    } finally {
      onDone()
    }
  }

  return (
    <AlertDialog>
      <AlertDialogTrigger render={<Button size="xs" variant="destructive" />}>
        <Trash2Icon /> {t('Delete')}
      </AlertDialogTrigger>
      <AlertDialogContent>
        <AlertDialogHeader>
          <AlertDialogTitle>{t('Delete webhook "{name}"?', { name: hook.name })}</AlertDialogTitle>
          <AlertDialogDescription className="[overflow-wrap:anywhere]">
            {t('No further events will be delivered to {url}.', { url: hook.url })}
          </AlertDialogDescription>
        </AlertDialogHeader>
        <AlertDialogFooter>
          <AlertDialogCancel>{t('Cancel')}</AlertDialogCancel>
          <AlertDialogAction
            className="bg-destructive/10 text-destructive hover:bg-destructive/20"
            onClick={() => void remove()}
          >
            {t('Delete')}
          </AlertDialogAction>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  )
}

function DeliveriesTable() {
  const { t } = useI18n()
  const canMutate = useHasRole('operator')
  const { data: deliveries, refresh } = usePoll(api.webhookDeliveries, 10_000)
  const [busyId, setBusyId] = useState<string | null>(null)

  const redeliver = async (d: WebhookDelivery) => {
    setBusyId(d.id)
    try {
      await api.redeliverWebhook(d.id)
      toast.success(t('Redelivery of "{event}" queued', { event: d.event }))
      refresh()
    } catch (e) {
      toast.error(e instanceof ApiError ? e.message : String(e))
    } finally {
      setBusyId(null)
    }
  }

  return (
    <>
      <SectionHeader title={t('Recent deliveries')} />
      <RecordList>
        {deliveries === null ? (
          <RecordSkeleton rows={3} />
        ) : deliveries.length === 0 ? (
          <RecordEmpty>{t('No deliveries yet')}</RecordEmpty>
        ) : (
          deliveries.map((d) => (
            <RecordRow
              key={d.id}
              title={
                <>
                  <TintBadge tint="lime">{d.event}</TintBadge>
                  {d.webhook_name}
                  {d.success ? (
                    <TintBadge tint="green">{d.status ?? 200}</TintBadge>
                  ) : (
                    <TintBadge tint="red">{d.status ?? t('failed')}</TintBadge>
                  )}
                </>
              }
              actions={
                canMutate ? (
                  <Button
                    size="xs"
                    variant="outline"
                    disabled={busyId === d.id}
                    onClick={() => void redeliver(d)}
                  >
                    {busyId === d.id ? <Spinner /> : <RotateCwIcon />} {t('Redeliver')}
                  </Button>
                ) : null
              }
            >
              <RecordFact icon={<ClockIcon />}>{formatRelativeTime(d.timestamp, t)}</RecordFact>
              <RecordFact icon={<RotateCwIcon />}>
                {t('{count} attempts', { count: d.attempts })}
              </RecordFact>
              {d.error && (
                <RecordFact className="basis-full text-destructive" title={d.error}>
                  {d.error}
                </RecordFact>
              )}
            </RecordRow>
          ))
        )}
      </RecordList>
    </>
  )
}

export function WebhooksSection() {
  const { t } = useI18n()
  const canMutate = useHasRole('operator')
  const { data: hooks, refresh } = usePoll(api.webhooks, 15_000)

  return (
    <section className="flex flex-col gap-3">
      <SectionHeader title={t('Webhooks')}>
        {canMutate && <CreateWebhookDialog onCreated={refresh} />}
      </SectionHeader>
      <RecordList>
        {hooks === null ? (
          <RecordSkeleton rows={3} />
        ) : hooks.length === 0 ? (
          <RecordEmpty>{t('No webhooks defined')}</RecordEmpty>
        ) : (
          hooks.map((h) => (
            <RecordRow
              key={h.id}
              title={
                <>
                  {h.name}
                  {h.format !== 'generic' && <TintBadge tint="blue">{h.format}</TintBadge>}
                  {h.signed && <TintBadge tint="green">{t('signed')}</TintBadge>}
                </>
              }
              actions={
                canMutate ? (
                  <>
                    <TestWebhookButton hook={h} onDone={refresh} />
                    <DeleteWebhookButton hook={h} onDone={refresh} />
                  </>
                ) : null
              }
            >
              <RecordFact className="basis-full font-mono" title={h.url}>
                {h.url}
              </RecordFact>
              <div className="flex flex-wrap gap-1">
                {(h.events.length ? h.events : ['*']).map((e) => (
                  <TintBadge key={e} tint="lime">
                    {e}
                  </TintBadge>
                ))}
              </div>
            </RecordRow>
          ))
        )}
      </RecordList>
      <DeliveriesTable />
    </section>
  )
}
