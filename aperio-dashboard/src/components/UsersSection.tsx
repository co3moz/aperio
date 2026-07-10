import { KeyRoundIcon, PencilIcon, PlusIcon, Trash2Icon } from 'lucide-react'
import { useState } from 'react'
import { toast } from 'sonner'
import { EmptyRow, SectionHeader, SkeletonRows } from './shared'
import { TintBadge, type Tint } from './badges'
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
import { Switch } from '@/components/ui/switch'
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
import { api, ApiError, type DashboardUser, type Role } from '@/lib/api'
import { formatRelativeTime } from '@/lib/format'
import { useSession } from '@/lib/session'

const ROLE_TINT: Record<Role, Tint> = { admin: 'red', operator: 'blue', viewer: 'gray' }

function RoleBadge({ role }: { role: Role }) {
  const { t } = useI18n()
  const LABEL: Record<Role, string> = { admin: t('Admin'), operator: t('Operator'), viewer: t('Viewer') }
  return <TintBadge tint={ROLE_TINT[role]}>{LABEL[role]}</TintBadge>
}

function RoleSelect({ value, onChange }: { value: Role; onChange: (r: Role) => void }) {
  const { t } = useI18n()
  return (
    <Select value={value} onValueChange={(v) => onChange(v as Role)}>
      <SelectTrigger className="w-full">
        <SelectValue />
      </SelectTrigger>
      <SelectContent>
        <SelectItem value="viewer">{t('Viewer')}</SelectItem>
        <SelectItem value="operator">{t('Operator')}</SelectItem>
        <SelectItem value="admin">{t('Admin')}</SelectItem>
      </SelectContent>
    </Select>
  )
}

function CreateUserDialog({ onCreated }: { onCreated: () => void }) {
  const { t } = useI18n()
  const [open, setOpen] = useState(false)
  const [username, setUsername] = useState('')
  const [password, setPassword] = useState('')
  const [role, setRole] = useState<Role>('viewer')
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)

  const openDialog = (next: boolean) => {
    if (next) {
      setUsername('')
      setPassword('')
      setRole('viewer')
      setError(null)
    }
    setOpen(next)
  }

  const submit = async () => {
    setBusy(true)
    setError(null)
    try {
      await api.createUser({ username: username.trim(), password, role })
      setOpen(false)
      toast.success(t('User "{name}" created', { name: username.trim() }))
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
        <PlusIcon /> {t('Add User')}
      </DialogTrigger>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{t('Add dashboard user')}</DialogTitle>
          <DialogDescription>
            {t('Users sign in at the dashboard login with their username and password.')}
          </DialogDescription>
        </DialogHeader>
        <div className="grid gap-4">
          <div className="grid gap-2">
            <Label htmlFor="user-name">{t('Username')}</Label>
            <Input
              id="user-name"
              value={username}
              onChange={(e) => setUsername(e.target.value)}
              placeholder="alice"
              autoComplete="off"
            />
          </div>
          <div className="grid gap-2">
            <Label htmlFor="user-pass">{t('Password (min. 8 characters)')}</Label>
            <Input
              id="user-pass"
              type="password"
              value={password}
              onChange={(e) => setPassword(e.target.value)}
              autoComplete="new-password"
            />
          </div>
          <div className="grid gap-2">
            <Label>{t('Role')}</Label>
            <RoleSelect value={role} onChange={setRole} />
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

function EditUserDialog({ user, onSaved }: { user: DashboardUser; onSaved: () => void }) {
  const { t } = useI18n()
  const [open, setOpen] = useState(false)
  const [role, setRole] = useState<Role>(user.role)
  const [enabled, setEnabled] = useState(user.enabled)
  const [password, setPassword] = useState('')
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)

  const openDialog = (next: boolean) => {
    if (next) {
      setRole(user.role)
      setEnabled(user.enabled)
      setPassword('')
      setError(null)
    }
    setOpen(next)
  }

  const submit = async () => {
    setBusy(true)
    setError(null)
    try {
      await api.updateUser(user.id, {
        role,
        enabled,
        ...(password.trim() ? { password } : {}),
      })
      setOpen(false)
      toast.success(t('User "{name}" updated', { name: user.username }))
      onSaved()
    } catch (e) {
      setError(e instanceof ApiError ? e.message : String(e))
    } finally {
      setBusy(false)
    }
  }

  return (
    <Dialog open={open} onOpenChange={openDialog}>
      <DialogTrigger render={<Button size="xs" variant="outline" />}>
        <PencilIcon /> {t('Edit')}
      </DialogTrigger>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{t('Edit user "{name}"', { name: user.username })}</DialogTitle>
        </DialogHeader>
        <div className="grid gap-4">
          <div className="grid gap-2">
            <Label>{t('Role')}</Label>
            <RoleSelect value={role} onChange={setRole} />
          </div>
          <label className="flex items-center justify-between gap-3 rounded-3xl border px-4 py-3">
            <span className="text-sm font-medium">{t('Account enabled')}</span>
            <Switch checked={enabled} onCheckedChange={setEnabled} />
          </label>
          <div className="grid gap-2">
            <Label htmlFor="user-newpass">{t('New password (leave blank to keep)')}</Label>
            <Input
              id="user-newpass"
              type="password"
              value={password}
              onChange={(e) => setPassword(e.target.value)}
              autoComplete="new-password"
            />
          </div>
          {error && <p className="text-sm text-destructive">{error}</p>}
        </div>
        <DialogFooter>
          <Button variant="outline" onClick={() => setOpen(false)}>
            {t('Cancel')}
          </Button>
          <Button onClick={submit} disabled={busy}>
            {busy && <Spinner />} {t('Save')}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

function DeleteUserButton({ user, onDone }: { user: DashboardUser; onDone: () => void }) {
  const { t } = useI18n()
  const remove = async () => {
    try {
      await api.deleteUser(user.id)
      toast.info(t('User "{name}" deleted', { name: user.username }))
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
          <AlertDialogTitle>{t('Delete user "{name}"?', { name: user.username })}</AlertDialogTitle>
          <AlertDialogDescription>
            {t('Their active dashboard sessions are ended immediately.')}
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

export function UsersSection() {
  const { t } = useI18n()
  const { username: self } = useSession()
  const { data: users, refresh } = usePoll(api.users, 15_000)

  return (
    <section className="flex flex-col gap-3">
      <SectionHeader
        title={t('Dashboard Users')}
        description={t('Role-based access. The master token and dashboard password always sign in as a built-in admin ("aperio").')}
      >
        <CreateUserDialog onCreated={refresh} />
      </SectionHeader>
      <Card className="overflow-hidden py-0">
        <Table>
          <TableHeader>
            <TableRow>
              <TableHead>{t('Username')}</TableHead>
              <TableHead>{t('Role')}</TableHead>
              <TableHead>{t('Status')}</TableHead>
              <TableHead>{t('Created')}</TableHead>
              <TableHead className="text-right">{t('Actions')}</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {users === null ? (
              <SkeletonRows rows={3} cols={5} />
            ) : users.length === 0 ? (
              <EmptyRow colSpan={5} icon={<KeyRoundIcon />}>
                {t('No dashboard users yet — the master token and dashboard password still work.')}
              </EmptyRow>
            ) : (
              users.map((u) => (
                <TableRow key={u.id}>
                  <TableCell className="font-medium">
                    {u.username}
                    {u.username === self && (
                      <span className="ml-1.5 text-xs text-muted-foreground">{t('(you)')}</span>
                    )}
                  </TableCell>
                  <TableCell>
                    <RoleBadge role={u.role} />
                  </TableCell>
                  <TableCell>
                    {u.enabled ? (
                      <TintBadge tint="green">{t('active')}</TintBadge>
                    ) : (
                      <TintBadge tint="gray">{t('disabled')}</TintBadge>
                    )}
                  </TableCell>
                  <TableCell className="text-sm text-muted-foreground">
                    {formatRelativeTime(u.created_at)}
                  </TableCell>
                  <TableCell>
                    <div className="flex justify-end gap-2">
                      <EditUserDialog user={u} onSaved={refresh} />
                      <DeleteUserButton user={u} onDone={refresh} />
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
