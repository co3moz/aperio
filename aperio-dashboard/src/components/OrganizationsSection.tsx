import {
  Building2Icon,
  GaugeIcon,
  KeyRoundIcon,
  PlusIcon,
  Trash2Icon,
  UsersIcon,
} from 'lucide-react'
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
import { useI18n } from '@/i18n'
import { api, ApiError, type Organization, type OrgUsage } from '@/lib/api'
import { formatRelativeTime } from '@/lib/format'

function CreateOrgDialog({ onCreated }: { onCreated: () => void }) {
  const { t } = useI18n()
  const [open, setOpen] = useState(false)
  const [name, setName] = useState('')
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)

  const openDialog = (next: boolean) => {
    if (next) {
      setName('')
      setError(null)
    }
    setOpen(next)
  }

  const submit = async () => {
    setBusy(true)
    setError(null)
    try {
      await api.createOrg(name.trim())
      setOpen(false)
      toast.success(t('Organization "{name}" created', { name: name.trim() }))
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
        <PlusIcon /> {t('New Organization')}
      </DialogTrigger>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{t('Create organization')}</DialogTitle>
          <DialogDescription>
            {t('Tokens and users you create while an organization is selected belong only to it — its members never see another org’s clients or tokens.')}
          </DialogDescription>
        </DialogHeader>
        <div className="grid gap-4">
          <div className="grid gap-2">
            <Label htmlFor="org-name">{t('Name')}</Label>
            <Input
              id="org-name"
              value={name}
              onChange={(e) => setName(e.target.value)}
              placeholder="Acme Inc."
              autoComplete="off"
              onKeyDown={(e) => {
                if (e.key === 'Enter' && name.trim() && !busy) void submit()
              }}
            />
          </div>
          {error && <p className="text-sm text-destructive">{error}</p>}
        </div>
        <DialogFooter>
          <Button variant="outline" onClick={() => setOpen(false)}>
            {t('Cancel')}
          </Button>
          <Button onClick={submit} disabled={busy || !name.trim()}>
            {busy && <Spinner />} {t('Create')}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

function DeleteOrgButton({ org, onDone }: { org: Organization; onDone: () => void }) {
  const { t } = useI18n()
  const nonEmpty = org.users > 0 || org.tokens > 0
  const remove = async () => {
    try {
      await api.deleteOrg(org.id)
      toast.info(t('Organization "{name}" deleted', { name: org.name }))
      onDone()
    } catch (e) {
      toast.error(e instanceof ApiError ? e.message : String(e))
    }
  }
  return (
    <AlertDialog>
      <AlertDialogTrigger render={<Button size="xs" variant="destructive" disabled={nonEmpty} />}>
        <Trash2Icon /> {t('Delete')}
      </AlertDialogTrigger>
      <AlertDialogContent>
        <AlertDialogHeader>
          <AlertDialogTitle>{t('Delete organization "{name}"?', { name: org.name })}</AlertDialogTitle>
          <AlertDialogDescription>
            {t('This cannot be undone. An organization can only be deleted once all its users and tokens are removed.')}
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

// Per-org quota editor + current-month usage. Opens on demand and fetches
// usage; saving updates the quota and re-fetches.
function QuotaDialog({ org }: { org: Organization }) {
  const { t } = useI18n()
  const [open, setOpen] = useState(false)
  const [usage, setUsage] = useState<OrgUsage | null>(null)
  const [form, setForm] = useState({ clients: '', tokens: '', users: '', bytesMb: '' })
  const [busy, setBusy] = useState(false)

  const load = async () => {
    const u = await api.orgUsage(org.id)
    setUsage(u)
    setForm({
      clients: u.quota?.max_clients != null ? String(u.quota.max_clients) : '',
      tokens: u.quota?.max_tokens != null ? String(u.quota.max_tokens) : '',
      users: u.quota?.max_users != null ? String(u.quota.max_users) : '',
      bytesMb:
        u.quota?.max_bytes_month != null
          ? String(Math.round(u.quota.max_bytes_month / (1024 * 1024)))
          : '',
    })
  }

  const onOpenChange = (next: boolean) => {
    setOpen(next)
    if (next) load().catch(() => setUsage(null))
  }

  // Empty input = clear the quota (send 0); a number sets it.
  const num = (s: string) => {
    const n = parseInt(s, 10)
    return Number.isNaN(n) || n < 0 ? 0 : n
  }

  const save = async () => {
    setBusy(true)
    try {
      await api.setOrgQuota(org.id, {
        max_clients: num(form.clients),
        max_tokens: num(form.tokens),
        max_users: num(form.users),
        max_bytes_month: num(form.bytesMb) * 1024 * 1024,
      })
      await load()
      toast.success(t('Quota updated'))
    } catch (e) {
      toast.error(e instanceof Error ? e.message : String(e))
    } finally {
      setBusy(false)
    }
  }

  const mb = (bytes: number) => `${(bytes / (1024 * 1024)).toFixed(1)} MB`

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogTrigger
        render={<Button variant="ghost" size="icon" aria-label={t('Quota & usage')} />}
      >
        <GaugeIcon />
      </DialogTrigger>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{t('Quota & usage — {name}', { name: org.name })}</DialogTitle>
          <DialogDescription>
            {t('Leave a field empty for no limit. Usage is for the current calendar month.')}
          </DialogDescription>
        </DialogHeader>
        {usage && (
          <div className="rounded-md border p-3 text-sm text-muted-foreground">
            {t('This month: {req} requests, {bytes}, {clients} clients, {tokens} tokens, {users} users', {
              req: usage.requests,
              bytes: mb(usage.bytes),
              clients: usage.clients,
              tokens: usage.tokens,
              users: usage.users,
            })}
          </div>
        )}
        <div className="grid grid-cols-2 gap-3">
          <div className="space-y-1">
            <Label>{t('Max clients')}</Label>
            <Input
              value={form.clients}
              onChange={(e) => setForm((f) => ({ ...f, clients: e.target.value }))}
              inputMode="numeric"
            />
          </div>
          <div className="space-y-1">
            <Label>{t('Max tokens')}</Label>
            <Input
              value={form.tokens}
              onChange={(e) => setForm((f) => ({ ...f, tokens: e.target.value }))}
              inputMode="numeric"
            />
          </div>
          <div className="space-y-1">
            <Label>{t('Max users')}</Label>
            <Input
              value={form.users}
              onChange={(e) => setForm((f) => ({ ...f, users: e.target.value }))}
              inputMode="numeric"
            />
          </div>
          <div className="space-y-1">
            <Label>{t('Max MB / month')}</Label>
            <Input
              value={form.bytesMb}
              onChange={(e) => setForm((f) => ({ ...f, bytesMb: e.target.value }))}
              inputMode="numeric"
            />
          </div>
        </div>
        <DialogFooter>
          <Button variant="outline" onClick={() => setOpen(false)}>
            {t('Close')}
          </Button>
          <Button onClick={save} disabled={busy}>
            {busy && <Spinner />} {t('Save quota')}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

export function OrganizationsSection() {
  const { t } = useI18n()
  const { data: orgs, refresh } = usePoll(api.orgs, 30_000)

  return (
    <section className="flex flex-col gap-3">
      <SectionHeader
        title={t('Organizations')}
        description={t('Isolated tenants. Switch into an organization from the sidebar to manage its own tokens, users, and clients. The master organization is implicit — everything created without an organization belongs to it.')}
      >
        <CreateOrgDialog onCreated={refresh} />
      </SectionHeader>
      <Card className="overflow-hidden py-0">
        <Table>
          <TableHeader>
            <TableRow>
              <TableHead>{t('Name')}</TableHead>
              <TableHead>{t('Users')}</TableHead>
              <TableHead>{t('Tokens')}</TableHead>
              <TableHead>{t('Created')}</TableHead>
              <TableHead className="text-right">{t('Actions')}</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {orgs === null ? (
              <SkeletonRows rows={2} cols={5} />
            ) : orgs.length === 0 ? (
              <EmptyRow colSpan={5} icon={<Building2Icon />}>
                {t('No organizations yet')}
              </EmptyRow>
            ) : (
              orgs.map((o) => (
                <TableRow key={o.id}>
                  <TableCell className="font-medium">
                    <div className="flex items-center gap-1.5">
                      <Building2Icon className="size-4 text-muted-foreground" />
                      {o.name}
                      {o.master && <TintBadge tint="lime">{t('master')}</TintBadge>}
                    </div>
                  </TableCell>
                  <TableCell>
                    <span className="inline-flex items-center gap-1 text-muted-foreground">
                      <UsersIcon className="size-3.5" /> {o.users}
                    </span>
                  </TableCell>
                  <TableCell>
                    <span className="inline-flex items-center gap-1 text-muted-foreground">
                      <KeyRoundIcon className="size-3.5" /> {o.tokens}
                    </span>
                  </TableCell>
                  <TableCell className="text-sm text-muted-foreground">
                    {o.created_at ? formatRelativeTime(o.created_at) : '-'}
                  </TableCell>
                  <TableCell>
                    <div className="flex justify-end">
                      {o.master ? (
                        <span className="text-muted-foreground">-</span>
                      ) : (
                        <div className="flex items-center gap-1">
                          <QuotaDialog org={o} />
                          <DeleteOrgButton org={o} onDone={refresh} />
                        </div>
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
