import { PlusIcon, Trash2Icon } from 'lucide-react'
import { useState } from 'react'
import { toast } from 'sonner'
import { CopyButton, EmptyRow, SectionHeader, SkeletonRows } from './shared'
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
import { api, type AdminKeyView } from '@/lib/api'
import { formatExpiry } from '@/lib/format'
import { useI18n } from '@/i18n'

const ROLES = ['viewer', 'operator', 'admin'] as const

// Shows the freshly minted secret exactly once, with a copy button.
function CreatedKeyDialog({ secret, onClose }: { secret: string | null; onClose: () => void }) {
  const { t } = useI18n()
  return (
    <Dialog open={secret !== null} onOpenChange={(o) => !o && onClose()}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{t('Admin key created')}</DialogTitle>
          <DialogDescription>
            {t('Copy it now — it is shown only once and cannot be retrieved later.')}
          </DialogDescription>
        </DialogHeader>
        <div className="flex items-center gap-2">
          <code className="flex-1 overflow-x-auto rounded bg-muted px-2 py-1 text-sm">{secret}</code>
          {secret && <CopyButton value={secret} />}
        </div>
        <DialogFooter>
          <Button onClick={onClose}>{t('Done')}</Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

function CreateAdminKeyDialog({
  onCreated,
  onSecret,
}: {
  onCreated: () => void
  onSecret: (s: string) => void
}) {
  const { t } = useI18n()
  const [open, setOpen] = useState(false)
  const [name, setName] = useState('')
  const [role, setRole] = useState<string>('operator')
  const [org, setOrg] = useState('')
  const [ttlDays, setTtlDays] = useState('')
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)

  const submit = async () => {
    if (!name.trim()) {
      setError(t('Key name is required'))
      return
    }
    setBusy(true)
    setError(null)
    const days = parseInt(ttlDays, 10)
    try {
      const created = await api.createAdminKey({
        name: name.trim(),
        role,
        ...(org.trim() ? { org_id: org.trim() } : {}),
        ...(Number.isNaN(days) || days <= 0 ? {} : { ttl_seconds: days * 86400 }),
      })
      onSecret(created.key)
      onCreated()
      setOpen(false)
      setName('')
      setOrg('')
      setTtlDays('')
      toast.success(t('Admin key "{name}" created', { name: name.trim() }))
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e))
    } finally {
      setBusy(false)
    }
  }

  return (
    <Dialog open={open} onOpenChange={setOpen}>
      <DialogTrigger render={<Button size="sm" />}>
        <PlusIcon /> {t('New admin key')}
      </DialogTrigger>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{t('New admin key')}</DialogTitle>
          <DialogDescription>
            {t('A scoped, revocable Bearer credential for automation — no master token needed.')}
          </DialogDescription>
        </DialogHeader>
        <div className="space-y-3">
          <div className="space-y-1">
            <Label>{t('Name')}</Label>
            <Input value={name} onChange={(e) => setName(e.target.value)} placeholder="ci-deploy" />
          </div>
          <div className="space-y-1">
            <Label>{t('Role')}</Label>
            <select
              className="w-full rounded-md border bg-background px-3 py-2 text-sm"
              value={role}
              onChange={(e) => setRole(e.target.value)}
            >
              {ROLES.map((r) => (
                <option key={r} value={r}>
                  {r}
                </option>
              ))}
            </select>
          </div>
          <div className="space-y-1">
            <Label>{t('Organization id (empty = master)')}</Label>
            <Input value={org} onChange={(e) => setOrg(e.target.value)} placeholder="" />
          </div>
          <div className="space-y-1">
            <Label>{t('Expires in days (empty = never)')}</Label>
            <Input
              value={ttlDays}
              onChange={(e) => setTtlDays(e.target.value)}
              inputMode="numeric"
              placeholder=""
            />
          </div>
          {error && <p className="text-sm text-destructive">{error}</p>}
        </div>
        <DialogFooter>
          <Button variant="outline" onClick={() => setOpen(false)}>
            {t('Cancel')}
          </Button>
          <Button onClick={submit} disabled={busy}>
            {busy && <Spinner />} {t('Create')}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

export function AdminKeysSection() {
  const { t } = useI18n()
  const { data, loading, refresh } = usePoll<AdminKeyView[]>(() => api.adminKeys(), 30000)
  const [secret, setSecret] = useState<string | null>(null)

  const revoke = async (id: string, name: string) => {
    try {
      await api.revokeAdminKey(id)
      refresh()
      toast.success(t('Admin key "{name}" revoked', { name }))
    } catch (e) {
      toast.error(e instanceof Error ? e.message : String(e))
    }
  }

  return (
    <Card className="p-4">
      <SectionHeader
        title={t('Programmatic admin keys')}
        description={t('Scoped Bearer credentials for automation (master-admin only).')}
      >
        <CreateAdminKeyDialog onCreated={refresh} onSecret={setSecret} />
      </SectionHeader>
      <Table>
        <TableHeader>
          <TableRow>
            <TableHead>{t('Name')}</TableHead>
            <TableHead>{t('Prefix')}</TableHead>
            <TableHead>{t('Role')}</TableHead>
            <TableHead>{t('Organization')}</TableHead>
            <TableHead>{t('Expires')}</TableHead>
            <TableHead />
          </TableRow>
        </TableHeader>
        <TableBody>
          {loading && !data ? (
            <SkeletonRows rows={2} cols={6} />
          ) : !data || data.length === 0 ? (
            <EmptyRow colSpan={6}>{t('No admin keys yet')}</EmptyRow>
          ) : (
            data.map((k) => (
              <TableRow key={k.id}>
                <TableCell className="font-medium">{k.name}</TableCell>
                <TableCell>
                  <code className="text-xs">{k.key_prefix}…</code>
                </TableCell>
                <TableCell>
                  <TintBadge tint={k.role === 'admin' ? 'red' : k.role === 'operator' ? 'amber' : 'gray'}>
                    {k.role}
                  </TintBadge>
                </TableCell>
                <TableCell>{k.org_id ?? t('master')}</TableCell>
                <TableCell className={k.expired ? 'text-destructive' : undefined}>
                  {formatExpiry(k.expires_at, k.expired)}
                </TableCell>
                <TableCell className="text-right">
                  <AlertDialog>
                    <AlertDialogTrigger
                      render={<Button variant="ghost" size="icon" aria-label={t('Revoke')} />}
                    >
                      <Trash2Icon className="text-destructive" />
                    </AlertDialogTrigger>
                    <AlertDialogContent>
                      <AlertDialogHeader>
                        <AlertDialogTitle>{t('Revoke admin key?')}</AlertDialogTitle>
                        <AlertDialogDescription>
                          {t('Automation using "{name}" will immediately lose access.', {
                            name: k.name,
                          })}
                        </AlertDialogDescription>
                      </AlertDialogHeader>
                      <AlertDialogFooter>
                        <AlertDialogCancel>{t('Cancel')}</AlertDialogCancel>
                        <AlertDialogAction onClick={() => revoke(k.id, k.name)}>
                          {t('Revoke')}
                        </AlertDialogAction>
                      </AlertDialogFooter>
                    </AlertDialogContent>
                  </AlertDialog>
                </TableCell>
              </TableRow>
            ))
          )}
        </TableBody>
      </Table>
      <CreatedKeyDialog secret={secret} onClose={() => setSecret(null)} />
    </Card>
  )
}
