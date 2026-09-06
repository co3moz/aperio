import {
  Building2Icon,
  SearchIcon,
  ClockIcon,
  GlobeIcon,
  KeyRoundIcon,
  LogOutIcon,
  MonitorIcon,
  PencilIcon,
  PlusIcon,
  Trash2Icon,
  XIcon,
} from 'lucide-react'
import { useEffect, useState } from 'react'
import { toast } from 'sonner'
import {
  RecordEmpty,
  RecordFact,
  RecordList,
  RecordRow,
  RecordSkeleton,
  SectionHeader,
  submitOnEnter,
} from './shared'
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
import { useStream } from '@/hooks/useStream'
import { useI18n } from '@/i18n'
import { usePaneFocus } from '@/lib/paneFocus'
import {
  api,
  ApiError,
  type DashboardUser,
  type Role,
  type LiveSession,
  type UserGrant,
} from '@/lib/api'
import { formatRelativeTime } from '@/lib/format'
import { useOrgLabel, useOrgName, useSession } from '@/lib/session'

/** The server's own floor, mirrored so the form can say no first. */
const MIN_PASSWORD = 8

const ROLE_TINT: Record<Role, Tint> = { admin: 'red', operator: 'blue', viewer: 'gray' }

function RoleBadge({ role }: { role: Role }) {
  const { t } = useI18n()
  const LABEL: Record<Role, string> = { admin: t('Admin'), operator: t('Operator'), viewer: t('Viewer') }
  return <TintBadge tint={ROLE_TINT[role]}>{LABEL[role]}</TintBadge>
}

function RoleSelect({ value, onChange }: { value: Role; onChange: (r: Role) => void }) {
  const { t } = useI18n()
  // `items` is what makes the closed trigger read the *label*: without it the
  // popup offered "İzleyici" and the field above it then said "viewer".
  const items: Record<Role, string> = {
    viewer: t('Viewer'),
    operator: t('Operator'),
    admin: t('Admin'),
  }
  return (
    <Select items={items} value={value} onValueChange={(v) => onChange(v as Role)}>
      <SelectTrigger className="w-full">
        <SelectValue />
      </SelectTrigger>
      <SelectContent>
        {(Object.keys(items) as Role[]).map((r) => (
          <SelectItem key={r} value={r}>
            {items[r]}
          </SelectItem>
        ))}
      </SelectContent>
    </Select>
  )
}

/** The organizations the caller may grant a role in: every reachable one
 *  where they hold Admin, and `*` when they hold `*` Admin, since a grant is
 *  bounded by the granter's. The server refuses anything past this too; the
 *  list only keeps the form from offering what would be refused. */
function useGrantTargets(): { id: string; label: string }[] {
  const { t } = useI18n()
  const { orgs, allOrgs } = useSession()
  const targets = orgs
    .filter((o) => o.role === 'admin')
    .map((o) => ({
      id: o.id,
      label: o.id === 'master' ? t('master') : o.custom_name || o.name,
    }))
  return allOrgs ? [{ id: '*', label: t('Every organization (*)') }, ...targets] : targets
}

/** One row per organization the user reaches, with the role held there. A
 *  master-side form; inside a child organization the role select is the
 *  whole story, since a user created there is granted that organization
 *  only. */
function GrantsEditor({
  value,
  onChange,
}: {
  value: UserGrant[]
  onChange: (grants: UserGrant[]) => void
}) {
  const { t } = useI18n()
  const targets = useGrantTargets()
  const taken = new Set(value.map((g) => g.org))
  const free = targets.filter((tg) => !taken.has(tg.id))
  const items: Record<string, string> = Object.fromEntries(targets.map((tg) => [tg.id, tg.label]))
  const update = (i: number, next: Partial<UserGrant>) =>
    onChange(value.map((g, j) => (j === i ? { ...g, ...next } : g)))
  return (
    <div className="grid gap-2">
      <Label>{t('Grants')}</Label>
      {value.map((g, i) => (
        <div key={g.org} className="flex items-center gap-2">
          <Select
            items={items}
            value={g.org}
            onValueChange={(v) => update(i, { org: v as string })}
          >
            <SelectTrigger className="flex-1">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              {targets
                .filter((tg) => tg.id === g.org || !taken.has(tg.id))
                .map((tg) => (
                  <SelectItem key={tg.id} value={tg.id}>
                    {tg.label}
                  </SelectItem>
                ))}
            </SelectContent>
          </Select>
          <div className="w-36">
            <RoleSelect value={g.role} onChange={(r) => update(i, { role: r })} />
          </div>
          <Button
            size="icon"
            variant="ghost"
            aria-label={t('Remove grant')}
            disabled={value.length === 1}
            onClick={() => onChange(value.filter((_, j) => j !== i))}
          >
            <XIcon />
          </Button>
        </div>
      ))}
      <Button
        size="sm"
        variant="outline"
        className="justify-self-start"
        disabled={free.length === 0}
        onClick={() => onChange([...value, { org: free[0].id, role: 'viewer' }])}
      >
        <PlusIcon /> {t('Add grant')}
      </Button>
      <p className="text-xs text-muted-foreground">
        {t('A user granted several organizations is created here in master and managed only from master.')}
      </p>
    </div>
  )
}

function CreateUserDialog({ onCreated }: { onCreated: () => void }) {
  const { t } = useI18n()
  const { selectedOrg } = useSession()
  // Inside a child organization a user is granted that organization only, so
  // the role is the whole form; master gets the editor.
  const editGrants = selectedOrg === 'master'
  const [open, setOpen] = useState(false)
  const [username, setUsername] = useState('')
  const [password, setPassword] = useState('')
  // An account that signs in through the identity provider only: the
  // username is the email the provider vouches for, and there is no
  // password to type.
  const [sso, setSso] = useState(false)
  const [role, setRole] = useState<Role>('viewer')
  const [grants, setGrants] = useState<UserGrant[]>([{ org: 'master', role: 'viewer' }])
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)

  // The command palette can ask for this form by name, which is the whole
  // point of the shortcut: one step from anywhere to the thing that does it.
  const focus = usePaneFocus()
  useEffect(() => {
    if (focus?.target === 'new-user') openDialog(true)
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [focus])

  const openDialog = (next: boolean) => {
    if (next) {
      setUsername('')
      setPassword('')
      setSso(false)
      setRole('viewer')
      setGrants([{ org: 'master', role: 'viewer' }])
      setError(null)
    }
    setOpen(next)
  }

  // The server refuses a short password; saying so here costs a round-trip
  // less than being told after the click.
  const ready = username.trim().length > 0 && (sso || password.length >= MIN_PASSWORD)

  const submit = async () => {
    if (!ready || busy) return
    setBusy(true)
    setError(null)
    try {
      await api.createUser({
        username: username.trim(),
        ...(sso ? {} : { password }),
        ...(editGrants ? { grants } : { role }),
      })
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
        <div className="grid gap-4" onKeyDown={submitOnEnter(() => void submit())}>
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
          <label className="flex items-center justify-between gap-3 rounded-3xl border px-4 py-3">
            <span className="text-sm font-medium">
              {t('Signs in through the identity provider only (no password)')}
            </span>
            <Switch checked={sso} onCheckedChange={setSso} />
          </label>
          {!sso && (
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
          )}
          {editGrants ? (
            <GrantsEditor value={grants} onChange={setGrants} />
          ) : (
            <div className="grid gap-2">
              <Label>{t('Role')}</Label>
              <RoleSelect value={role} onChange={setRole} />
            </div>
          )}
          {error && <p className="text-sm text-destructive">{error}</p>}
        </div>
        <DialogFooter>
          <Button variant="outline" onClick={() => setOpen(false)}>
            {t('Cancel')}
          </Button>
          <Button onClick={submit} disabled={busy || !ready}>
            {busy && <Spinner />} {t('Create')}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

function EditUserDialog({ user, onSaved }: { user: DashboardUser; onSaved: () => void }) {
  const { t } = useI18n()
  const { selectedOrg } = useSession()
  const editGrants = selectedOrg === 'master'
  const [open, setOpen] = useState(false)
  const [role, setRole] = useState<Role>(user.role)
  const [grants, setGrants] = useState<UserGrant[]>(user.grants)
  const [enabled, setEnabled] = useState(user.enabled)
  const [password, setPassword] = useState('')
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)

  const openDialog = (next: boolean) => {
    if (next) {
      setRole(user.role)
      setGrants(user.grants)
      setEnabled(user.enabled)
      setPassword('')
      setError(null)
    }
    setOpen(next)
  }

  // Blank means "keep the current one"; anything else has to clear the same
  // bar a new account does.
  const ready = password === '' || password.length >= MIN_PASSWORD

  const submit = async () => {
    if (!ready || busy) return
    setBusy(true)
    setError(null)
    try {
      await api.updateUser(user.id, {
        ...(editGrants ? { grants } : { role }),
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
        <div className="grid gap-4" onKeyDown={submitOnEnter(() => void submit())}>
          {editGrants ? (
            <GrantsEditor value={grants} onChange={setGrants} />
          ) : (
            <div className="grid gap-2">
              <Label>{t('Role')}</Label>
              <RoleSelect value={role} onChange={setRole} />
            </div>
          )}
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
          <Button onClick={submit} disabled={busy || !ready}>
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

function ResetTotpButton({ user, onDone }: { user: DashboardUser; onDone: () => void }) {
  const { t } = useI18n()
  const reset = async () => {
    try {
      await api.totpAdminReset(user.id)
      toast.info(t('Two-factor auth reset for "{name}"', { name: user.username }))
    } catch (e) {
      toast.error(e instanceof ApiError ? e.message : String(e))
    } finally {
      onDone()
    }
  }

  return (
    <AlertDialog>
      <AlertDialogTrigger render={<Button size="xs" variant="outline" />}>
        {t('Reset 2FA')}
      </AlertDialogTrigger>
      <AlertDialogContent>
        <AlertDialogHeader>
          <AlertDialogTitle>{t('Reset two-factor auth for "{name}"?', { name: user.username })}</AlertDialogTitle>
          <AlertDialogDescription>
            {t('The user will sign in with their password only until they enroll again. Use this when someone lost their authenticator and recovery codes.')}
          </AlertDialogDescription>
        </AlertDialogHeader>
        <AlertDialogFooter>
          <AlertDialogCancel>{t('Cancel')}</AlertDialogCancel>
          <AlertDialogAction onClick={() => void reset()}>{t('Reset')}</AlertDialogAction>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  )
}


function SessionsCard() {
  const { t } = useI18n()
  const { data: sessions, refresh } = useStream('sessions', api.sessions, 15_000)
  const [busy, setBusy] = useState(false)

  const revoke = async (s: LiveSession) => {
    try {
      await api.revokeSession(s.id)
      toast.info(t('Session ended'))
    } catch (e) {
      toast.error(e instanceof ApiError ? e.message : String(e))
    } finally {
      refresh()
    }
  }
  const clearAll = async () => {
    setBusy(true)
    try {
      const res = await api.clearSessions()
      toast.info(t('{count} other session(s) ended', { count: String(res.ended) }))
    } catch (e) {
      toast.error(e instanceof ApiError ? e.message : String(e))
    } finally {
      setBusy(false)
      refresh()
    }
  }

  return (
    <>
      <SectionHeader
        title={t('Active sessions')}
        description={t('Everyone currently signed in, and where from. Ending a session signs that browser out on its next request.')}
      >
        <Button size="sm" variant="destructive" disabled={busy} onClick={() => void clearAll()}>
          <LogOutIcon /> {t('Sign out everywhere else')}
        </Button>
      </SectionHeader>
      <RecordList>
        {sessions === null ? (
          <RecordSkeleton rows={2} />
        ) : sessions.length === 0 ? (
          <RecordEmpty>{t('No live sessions')}</RecordEmpty>
        ) : (
          sessions.map((s) => (
            <RecordRow
              key={s.id}
              title={
                <>
                  {s.username}
                  <TintBadge tint="gray">{s.role}</TintBadge>
                  {s.current && <TintBadge tint="lime">{t('this session')}</TintBadge>}
                  {s.scope_host && <TintBadge tint="blue">{s.scope_host}</TintBadge>}
                </>
              }
              actions={
                s.current ? null : (
                  <Button size="xs" variant="destructive" onClick={() => void revoke(s)}>
                    <LogOutIcon /> {t('End session')}
                  </Button>
                )
              }
            >
              <RecordFact icon={<GlobeIcon />} className="font-mono">
                {s.ip ?? '-'}
              </RecordFact>
              {s.created_at && (
                <RecordFact icon={<ClockIcon />}>{formatRelativeTime(s.created_at, t)}</RecordFact>
              )}
              <RecordFact
                icon={<MonitorIcon />}
                className="basis-full"
                title={s.user_agent ?? undefined}
              >
                {s.user_agent ?? '-'}
              </RecordFact>
            </RecordRow>
          ))
        )}
      </RecordList>
    </>
  )
}

/** The grants beyond the home organization, as badges: what a reader of the
 *  list needs to see about a user who reaches more than one place. */
function GrantBadges({ user }: { user: DashboardUser }) {
  const { t } = useI18n()
  const label = useOrgLabel()
  const home = user.org_id ?? 'master'
  const LABEL: Record<Role, string> = { admin: t('Admin'), operator: t('Operator'), viewer: t('Viewer') }
  return (
    <>
      {user.grants
        .filter((g) => g.org !== home)
        .map((g) => (
          <span key={g.org} title={g.source ? `${t('Group')}: ${g.source}` : undefined}>
            <TintBadge tint={g.org === '*' ? 'red' : ROLE_TINT[g.role]}>
              <Building2Icon className="size-3" /> {label(g.org)} · {LABEL[g.role]}
              {g.source ? ' ·' : ''}
            </TintBadge>
          </span>
        ))}
    </>
  )
}

export function UsersSection() {
  const { t } = useI18n()
  const { username: self } = useSession()
  const orgName = useOrgName()
  const { data: users, refresh } = useStream('users', api.users, 15_000)
  const [search, setSearch] = useState('')
  const needle = search.trim().toLowerCase()
  const shown = users?.filter((u) => !needle || u.username.toLowerCase().includes(needle)) ?? null
  // Every `*` holder lives in master, so the master list is the whole set.
  const wide = users?.filter((u) => u.grants.some((g) => g.org === '*')).length ?? 0

  return (
    <section className="flex flex-col gap-3">
      <SectionHeader
        title={t('Dashboard Users')}
        description={t('Role-based access. The master token always signs in as the built-in admin ("aperio").')}
      >
        {/* Which org these are is not decoration: the list is filtered to the
            session's organization, so without it a short list reads as "there
            are only two users" rather than "in this org there are two". */}
        <TintBadge tint="blue">
          <Building2Icon className="size-3.5" /> {orgName}
        </TintBadge>
        <div className="relative">
          <SearchIcon className="pointer-events-none absolute left-2.5 top-1/2 size-4 -translate-y-1/2 text-muted-foreground" />
          <Input
            placeholder={t('Search users…')}
            value={search}
            onChange={(e) => setSearch(e.target.value)}
            className="w-48 pl-8"
          />
        </div>
        <CreateUserDialog onCreated={refresh} />
      </SectionHeader>
      {wide > 0 && (
        <p className="rounded-3xl border border-amber-500/30 bg-amber-500/10 px-4 py-3 text-sm">
          {t('{count} user(s) reach every organization (*). Narrow the ones that do not need it.', {
            count: String(wide),
          })}
        </p>
      )}
      <RecordList>
        {shown === null ? (
          <RecordSkeleton rows={3} />
        ) : shown.length === 0 ? (
          <RecordEmpty icon={<KeyRoundIcon />}>
            {needle ? t('Nothing matches "{search}"', { search }) : t('No dashboard users yet; the master token still signs in.')}
          </RecordEmpty>
        ) : (
          shown.map((u) => (
            <RecordRow
              key={u.id}
              title={
                <>
                  {u.username}
                  {u.username === self && (
                    <span className="text-xs font-normal text-muted-foreground">{t('(you)')}</span>
                  )}
                  <RoleBadge role={u.role} />
                  {u.sso && <TintBadge tint="blue">SSO</TintBadge>}
                  <GrantBadges user={u} />
                  {u.enabled ? (
                    <TintBadge tint="green">{t('active')}</TintBadge>
                  ) : (
                    <TintBadge tint="gray">{t('disabled')}</TintBadge>
                  )}
                  {u.totp && <TintBadge tint="blue">2FA</TintBadge>}
                </>
              }
              actions={
                <>
                  {u.totp && <ResetTotpButton user={u} onDone={refresh} />}
                  <EditUserDialog user={u} onSaved={refresh} />
                  <DeleteUserButton user={u} onDone={refresh} />
                </>
              }
            >
              <RecordFact icon={<ClockIcon />}>{formatRelativeTime(u.created_at, t)}</RecordFact>
            </RecordRow>
          ))
        )}
      </RecordList>
      <SessionsCard />
    </section>
  )
}
