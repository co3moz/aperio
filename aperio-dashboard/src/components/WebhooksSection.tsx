import { PlusIcon, Trash2Icon } from 'lucide-react'
import { useState } from 'react'
import { toast } from 'sonner'
import { EmptyRow, SectionHeader, SkeletonRows } from './shared'
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
import { Card } from '@/components/ui/card'
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
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table'
import { usePoll } from '@/hooks/usePoll'
import { api, ApiError, type Webhook } from '@/lib/api'
import { splitList } from '@/lib/format'
import { useI18n } from '@/i18n'
import { useHasRole } from '@/lib/session'

const KNOWN_EVENTS =
  'client_connected, client_disconnected, client_draining, token_created, token_revoked, tunnel_created, tunnel_deleted, share_created, maintenance_on, maintenance_off'

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
              value={url}
              onChange={(e) => setUrl(e.target.value)}
              placeholder="https://example.com/hooks/aperio"
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
            <Select value={format} onValueChange={(v) => setFormat(v as string)}>
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

function DeleteWebhookButton({ hook, onDone }: { hook: Webhook; onDone: () => void }) {
  const { t } = useI18n()
  const remove = async () => {
    try {
      await api.deleteWebhook(hook.id)
      toast.info(t('Webhook "{name}" deleted', { name: hook.name }))
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
          <AlertDialogDescription>
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

export function WebhooksSection() {
  const { t } = useI18n()
  const canMutate = useHasRole('operator')
  const { data: hooks, refresh } = usePoll(api.webhooks, 15_000)

  return (
    <section className="flex flex-col gap-3">
      <SectionHeader title={t('Webhooks')}>
        {canMutate && <CreateWebhookDialog onCreated={refresh} />}
      </SectionHeader>
      <Card className="overflow-hidden py-0">
        <Table>
          <TableHeader>
            <TableRow>
              <TableHead>{t('Name')}</TableHead>
              <TableHead>URL</TableHead>
              <TableHead>{t('Events')}</TableHead>
              <TableHead className="text-right">{t('Actions')}</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {hooks === null ? (
              <SkeletonRows rows={3} cols={4} />
            ) : hooks.length === 0 ? (
              <EmptyRow colSpan={4}>{t('No webhooks defined')}</EmptyRow>
            ) : (
              hooks.map((h) => (
                <TableRow key={h.id}>
                  <TableCell>
                    <div className="flex items-center gap-1.5 font-medium">
                      {h.name}
                      {h.format !== 'generic' && <TintBadge tint="blue">{h.format}</TintBadge>}
                      {h.signed && <TintBadge tint="green">{t('signed')}</TintBadge>}
                    </div>
                  </TableCell>
                  <TableCell>
                    <code className="break-all font-mono text-xs">{h.url}</code>
                  </TableCell>
                  <TableCell>
                    <div className="flex flex-wrap gap-1">
                      {(h.events.length ? h.events : ['*']).map((e) => (
                        <TintBadge key={e} tint="lime">
                          {e}
                        </TintBadge>
                      ))}
                    </div>
                  </TableCell>
                  <TableCell>
                    <div className="flex justify-end">
                      {canMutate ? (
                        <DeleteWebhookButton hook={h} onDone={refresh} />
                      ) : (
                        <span className="text-muted-foreground">-</span>
                      )}
                    </div>
                  </TableCell>
                </TableRow>
              ))
            )}
          </TableBody>
        </Table>
      </Card>
    </section>
  )
}
