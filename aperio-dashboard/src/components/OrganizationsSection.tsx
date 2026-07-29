import {
  Building2Icon,
  GlobeIcon,
  KeyRoundIcon,
  PencilIcon,
  PlusIcon,
  Trash2Icon,
  UsersIcon,
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
import { nameError, slug } from '@/lib/names'
import { Spinner } from '@/components/ui/spinner'
import { usePoll } from '@/hooks/usePoll'
import { useI18n } from '@/i18n'
import { usePaneFocus } from '@/lib/paneFocus'
import { api, ApiError, type Organization, type OrgUsage } from '@/lib/api'
import { formatRelativeTime } from '@/lib/format'

/** Splits a comma/whitespace separated hostname list into pattern entries. */
function parseHostnames(raw: string): string[] {
  return raw
    .split(/[,\s]+/)
    .map((h) => h.trim())
    .filter(Boolean)
}

/** The four caps, as text so an empty box can mean "no limit". */
interface QuotaForm {
  clients: string
  tokens: string
  users: string
  bytesMb: string
}

const EMPTY_QUOTA: QuotaForm = { clients: '', tokens: '', users: '', bytesMb: '' }

/** Empty input = clear the quota (the API reads 0 as "no limit"). */
function quotaNumber(s: string): number {
  const n = parseInt(s, 10)
  return Number.isNaN(n) || n < 0 ? 0 : n
}

function quotaPayload(f: QuotaForm) {
  return {
    max_clients: quotaNumber(f.clients),
    max_tokens: quotaNumber(f.tokens),
    max_users: quotaNumber(f.users),
    max_bytes_month: quotaNumber(f.bytesMb) * 1024 * 1024,
  }
}

/**
 * The quota inputs, shared by creating an organization and editing one.
 *
 * One component rather than two copies because the point of asking at
 * creation is that the caps are part of what an organization *is*; if the
 * create form could drift from the edit form, it would quietly stop offering
 * one of them.
 */
function QuotaFields({
  value,
  onChange,
}: {
  value: QuotaForm
  onChange: (next: QuotaForm) => void
}) {
  const { t } = useI18n()
  const set = (k: keyof QuotaForm) => (e: React.ChangeEvent<HTMLInputElement>) =>
    onChange({ ...value, [k]: e.target.value })

  return (
    <div className="grid gap-3 sm:grid-cols-2">
      <div className="space-y-1">
        <Label>{t('Max clients')}</Label>
        <Input value={value.clients} onChange={set('clients')} inputMode="numeric" />
      </div>
      <div className="space-y-1">
        <Label>{t('Max tokens')}</Label>
        <Input value={value.tokens} onChange={set('tokens')} inputMode="numeric" />
      </div>
      <div className="space-y-1">
        <Label>{t('Max users')}</Label>
        <Input value={value.users} onChange={set('users')} inputMode="numeric" />
      </div>
      <div className="space-y-1">
        <Label>{t('Max MB / month')}</Label>
        <Input value={value.bytesMb} onChange={set('bytesMb')} inputMode="numeric" />
      </div>
    </div>
  )
}

function CreateOrgDialog({ onCreated }: { onCreated: () => void }) {
  const { t } = useI18n()
  const [open, setOpen] = useState(false)
  const [name, setName] = useState('')
  const [customName, setCustomName] = useState('')
  const [hostnames, setHostnames] = useState('')
  const [quota, setQuota] = useState<QuotaForm>(EMPTY_QUOTA)
  const [nameEdited, setNameEdited] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)

  // The command palette can ask for this form by name, which is the whole
  // point of the shortcut: one step from anywhere to the thing that does it.
  const focus = usePaneFocus()
  useEffect(() => {
    if (focus?.target === 'new-org') openDialog(true)
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [focus])

  const openDialog = (next: boolean) => {
    if (next) {
      setName('')
      setCustomName('')
      setNameEdited(false)
      setHostnames('')
      setQuota(EMPTY_QUOTA)
      setError(null)
    }
    setOpen(next)
  }

  const submit = async () => {
    if (!name.trim() || busy) return
    // Checked here as well as on the server, so the rule is explained where
    // the mistake was made rather than as a failed request.
    const wrong = nameError('organization', name)
    if (wrong) {
      setError(wrong)
      return
    }
    setBusy(true)
    setError(null)
    const label = name.trim()
    try {
      const created = await api.createOrg(label, parseHostnames(hostnames), customName.trim())
      // The create endpoint takes the name and the fence; the caps are their
      // own endpoint. Only call it when something was actually typed, so a
      // form left blank does not write four explicit "no limit" values.
      if (Object.values(quota).some((v) => v.trim())) {
        try {
          await api.setOrgQuota(created.id, quotaPayload(quota))
        } catch (e) {
          // The organization exists; only the caps failed to land. Say so
          // rather than reporting a failure that would send someone looking
          // for an org that is already there — uncapped.
          setError(
            t('Organization "{name}" was created, but its limits could not be saved: {error}. Set them with Edit.', {
              name: label,
              error: e instanceof ApiError ? e.message : String(e),
            }),
          )
          onCreated()
          return
        }
      }
      setOpen(false)
      toast.success(t('Organization "{name}" created', { name: label }))
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
        <div className="grid gap-4" onKeyDown={submitOnEnter(() => void submit())}>
          <div className="grid gap-2">
            <Label htmlFor="org-custom-name">{t('Display name')}</Label>
            <Input
              id="org-custom-name"
              value={customName}
              onChange={(e) => {
                setCustomName(e.target.value)
                // The handle follows what is being typed until it is edited
                // by hand: the id nobody wants to think about writes itself,
                // and stays visible so it is never a surprise later.
                if (!nameEdited) setName(slug(e.target.value))
              }}
              placeholder="Acme Inc."
              autoComplete="off"
            />
            <p className="text-xs text-muted-foreground">
              {t('What this organization is called on screen. Any language, any punctuation, and changeable later.')}
            </p>
          </div>
          <div className="grid gap-2">
            <Label htmlFor="org-name">{t('Handle')}</Label>
            <Input
              id="org-name"
              value={name}
              onChange={(e) => {
                setNameEdited(true)
                setName(e.target.value)
              }}
              placeholder="acme"
              autoComplete="off"
              className="font-mono"
            />
            <p className="text-xs text-muted-foreground">
              {t('a-z, 0-9 and _ . This is what addresses the organization — in {example}, in a server’s expose: rule and in the API — so it is fixed once created.', { example: `${name || 'acme'}@postgres` })}
            </p>
          </div>
          <div className="grid gap-2">
            <Label htmlFor="org-hostnames">{t('Allowed hostnames (optional)')}</Label>
            <Input
              id="org-hostnames"
              value={hostnames}
              onChange={(e) => setHostnames(e.target.value)}
              placeholder="acme.com, *.acme.example.com"
              autoComplete="off"
            />
            <p className="text-xs text-muted-foreground">
              {t('Fences every bind made inside the organization: its tokens and clients can only claim these hostnames. Leave empty for no restriction.')}
            </p>
          </div>
          <div className="grid gap-2">
            <Label>{t('Limits (optional)')}</Label>
            <p className="text-xs text-muted-foreground">
              {t('Leave a field empty for no limit. The monthly allowance covers traffic proxied for this organization and resets each calendar month.')}
            </p>
            <QuotaFields value={quota} onChange={setQuota} />
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

// Everything about one organization that can change after it exists: its
// fence, its caps, and its SSO, next to what it has used this month. Opens on
// demand and fetches usage; saving writes and re-fetches.
function EditOrgDialog({ org, onSaved }: { org: Organization; onSaved: () => void }) {
  const { t } = useI18n()
  const [open, setOpen] = useState(false)
  const [customName, setCustomName] = useState(org.custom_name ?? '')
  const [usage, setUsage] = useState<OrgUsage | null>(null)
  const [form, setForm] = useState<QuotaForm>(EMPTY_QUOTA)
  const [hostnames, setHostnames] = useState('')
  const [busy, setBusy] = useState(false)

  const load = async () => {
    const u = await api.orgUsage(org.id)
    setUsage(u)
    setHostnames((u.hostnames ?? []).join(', '))
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
    if (next) {
      setCustomName(org.custom_name ?? '')
      load().catch(() => setUsage(null))
    }
  }

  const save = async () => {
    setBusy(true)
    try {
      // The display name is the one thing here that can change freely; the
      // handle is deliberately not editable, since an `expose:` rule and a
      // binder's config point at it from machines this screen cannot reach.
      if (customName.trim() !== (org.custom_name ?? '')) {
        await api.setOrgCustomName(org.id, customName.trim() || null)
      }
      await api.setOrgQuota(org.id, quotaPayload(form))
      // The allowlist is a separate endpoint; save it in the same click so the
      // dialog behaves as one form.
      await api.setOrgHostnames(org.id, parseHostnames(hostnames))
      await load()
      onSaved()
      toast.success(t('Organization updated'))
    } catch (e) {
      toast.error(e instanceof Error ? e.message : String(e))
    } finally {
      setBusy(false)
    }
  }

  const mb = (bytes: number) => `${(bytes / (1024 * 1024)).toFixed(1)} MB`

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogTrigger render={<Button variant="outline" size="xs" />}>
        <PencilIcon /> {t('Edit')}
      </DialogTrigger>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>
            {t('Edit organization "{name}"', { name: org.custom_name || org.name })}
          </DialogTitle>
          <DialogDescription>
            {t('Leave a limit empty for no limit. Usage is for the current calendar month.')}
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
        <div className="space-y-1">
          <Label htmlFor={`org-custom-${org.id}`}>{t('Display name')}</Label>
          <Input
            id={`org-custom-${org.id}`}
            value={customName}
            onChange={(e) => setCustomName(e.target.value)}
            placeholder={org.name}
            autoComplete="off"
          />
          <p className="text-xs text-muted-foreground">
            {t('Only what it is called. The handle {handle} is what addresses it and never changes.', { handle: org.name })}
          </p>
        </div>
        <QuotaFields value={form} onChange={setForm} />
        <div className="space-y-1">
          <Label>{t('Allowed hostnames')}</Label>
          <Input
            value={hostnames}
            onChange={(e) => setHostnames(e.target.value)}
            placeholder="acme.com, *.acme.example.com"
            autoComplete="off"
          />
          <p className="text-xs text-muted-foreground">
            {t('Only these hostnames may be bound by this organization. Empty = no restriction.')}
          </p>
        </div>
        <OidcForm org={org} />
        <DialogFooter>
          <Button variant="outline" onClick={() => setOpen(false)}>
            {t('Close')}
          </Button>
          <Button onClick={save} disabled={busy}>
            {busy && <Spinner />} {t('Save')}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

// Per-org OIDC SSO override: sign-in at /aperio/oidc/login?org=<id> binds the
// session to this org. The secret is write-only; empty issuer clears the config.
function OidcForm({ org }: { org: Organization }) {
  const { t } = useI18n()
  const [f, setF] = useState({ issuer: '', clientId: '', clientSecret: '', emails: '' })
  const [busy, setBusy] = useState(false)

  const save = async () => {
    setBusy(true)
    try {
      await api.setOrgOidc(org.id, {
        issuer: f.issuer.trim(),
        client_id: f.clientId.trim(),
        client_secret: f.clientSecret,
        allowed_emails: f.emails
          .split(',')
          .map((e) => e.trim())
          .filter(Boolean),
      })
      toast.success(f.issuer.trim() ? t('OIDC configured') : t('OIDC cleared'))
    } catch (e) {
      toast.error(e instanceof Error ? e.message : String(e))
    } finally {
      setBusy(false)
    }
  }

  return (
    <details className="rounded-md border p-3">
      <summary className="cursor-pointer text-sm font-medium">{t('OIDC SSO override')}</summary>
      <p className="mb-2 mt-1 text-xs text-muted-foreground">
        {t('Sign in at /aperio/oidc/login?org={id}. Leave issuer empty to clear.', { id: org.id })}
      </p>
      <div className="grid grid-cols-2 gap-2">
        <Input
          placeholder={t('Issuer URL')}
          value={f.issuer}
          onChange={(e) => setF((s) => ({ ...s, issuer: e.target.value }))}
        />
        <Input
          placeholder={t('Client id')}
          value={f.clientId}
          onChange={(e) => setF((s) => ({ ...s, clientId: e.target.value }))}
        />
        <Input
          type="password"
          // A client secret is not a login: a password manager offering to
          // save it, or filling it with something else, is only ever noise.
          autoComplete="off"
          placeholder={t('Client secret')}
          value={f.clientSecret}
          onChange={(e) => setF((s) => ({ ...s, clientSecret: e.target.value }))}
        />
        <Input
          placeholder={t('Allowed emails (comma)')}
          value={f.emails}
          onChange={(e) => setF((s) => ({ ...s, emails: e.target.value }))}
        />
      </div>
      <Button size="sm" className="mt-2" onClick={save} disabled={busy}>
        {busy && <Spinner />} {t('Save OIDC')}
      </Button>
    </details>
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
      <RecordList>
        {orgs === null ? (
          <RecordSkeleton rows={2} />
        ) : orgs.length === 0 ? (
          <RecordEmpty icon={<Building2Icon />}>{t('No organizations yet')}</RecordEmpty>
        ) : (
          orgs.map((o) => (
            <RecordRow
              key={o.id}
              title={
                <>
                  <Building2Icon className="size-4 text-muted-foreground" />
                  {o.custom_name || o.name}
                  {/* The handle, whenever it is not already what is shown: it
                      is what `payments@postgres` and an `expose:` rule name,
                      so it belongs on screen next to the label rather than
                      only in the API. */}
                  {o.custom_name && (
                    <span className="font-mono text-xs font-normal text-muted-foreground">
                      {o.name}
                    </span>
                  )}
                  {o.master && <TintBadge tint="lime">{t('master')}</TintBadge>}
                </>
              }
              actions={
                o.master ? null : (
                  <>
                    <EditOrgDialog org={o} onSaved={refresh} />
                    <DeleteOrgButton org={o} onDone={refresh} />
                  </>
                )
              }
            >
              <RecordFact icon={<UsersIcon />}>
                {t('{count} users', { count: o.users })}
              </RecordFact>
              <RecordFact icon={<KeyRoundIcon />}>
                {t('{count} tokens', { count: o.tokens })}
              </RecordFact>
              {o.created_at && <RecordFact>{formatRelativeTime(o.created_at)}</RecordFact>}
              <RecordFact
                icon={<GlobeIcon />}
                className="basis-full"
                title={o.master || !o.hostnames?.length ? t('No hostname restriction') : undefined}
              >
                {o.master || !o.hostnames?.length ? (
                  t('any hostname')
                ) : (
                  <span className="font-mono">{o.hostnames.join(', ')}</span>
                )}
              </RecordFact>
            </RecordRow>
          ))
        )}
      </RecordList>
    </section>
  )
}
