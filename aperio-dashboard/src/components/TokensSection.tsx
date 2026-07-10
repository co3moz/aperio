import { PencilIcon, PlusIcon, Trash2Icon } from 'lucide-react'
import { useState } from 'react'
import { toast } from 'sonner'
import { CopyButton, EmptyRow, SectionHeader, SkeletonRows } from './shared'
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
import { Checkbox } from '@/components/ui/checkbox'
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
import { api, ApiError, type TokenView } from '@/lib/api'
import { formatExpiry, splitList } from '@/lib/format'
import { cn } from '@/lib/utils'

function BadgeList({ items, fallback, tint }: { items: string[]; fallback: string; tint: Tint }) {
  const shown = items.length ? items : [fallback]
  return (
    <div className="flex flex-wrap gap-1">
      {shown.map((item) => (
        <TintBadge key={item} tint={tint}>
          {item}
        </TintBadge>
      ))}
    </div>
  )
}

interface TokenFormState {
  name: string
  hostnames: string
  paths: string
  ips: string
  ttl: string
  maxRps: string
  dailyMaxMb: string
  allowPublic: boolean
}

function formFromToken(t: TokenView | null): TokenFormState {
  return {
    name: t?.name ?? '',
    hostnames: t ? (t.hostnames.length ? t.hostnames : ['*']).join(', ') : '*',
    paths: t ? (t.paths.length ? t.paths : ['*']).join(', ') : '*',
    ips: t ? (t.allowed_ips.length ? t.allowed_ips : ['0.0.0.0/0']).join(', ') : '0.0.0.0/0',
    ttl: '',
    maxRps: t?.max_rps != null ? String(t.max_rps) : '',
    dailyMaxMb: t?.daily_max_bytes != null ? String(t.daily_max_bytes / (1024 * 1024)) : '',
    allowPublic: t?.allow_public ?? false,
  }
}

// Shared create/edit dialog. In edit mode the name is fixed and the TTL field
// means "new lifetime from now" (0 = never expires, empty = keep current).
function TokenFormDialog({
  editing,
  onSaved,
  onCreated,
}: {
  editing: TokenView | null
  onSaved: () => void
  onCreated: (secret: string) => void
}) {
  const [open, setOpen] = useState(false)
  const [form, setForm] = useState<TokenFormState>(formFromToken(editing))
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)

  const openDialog = (next: boolean) => {
    if (next) {
      setForm(formFromToken(editing))
      setError(null)
    }
    setOpen(next)
  }

  const set =
    (key: keyof TokenFormState) =>
    (e: React.ChangeEvent<HTMLInputElement>): void =>
      setForm((f) => ({ ...f, [key]: e.target.value }))

  const submit = async () => {
    setBusy(true)
    setError(null)
    const ttl = parseInt(form.ttl, 10)
    // Empty input = keep current (edit) / no limit (create); 0 clears on edit.
    const maxRps = form.maxRps.trim() === '' ? NaN : Number(form.maxRps)
    const dailyMb = form.dailyMaxMb.trim() === '' ? NaN : Number(form.dailyMaxMb)
    const dailyBytes = Number.isNaN(dailyMb) ? NaN : Math.round(dailyMb * 1024 * 1024)
    try {
      if (editing) {
        await api.updateToken(editing.id, {
          hostnames: splitList(form.hostnames),
          paths: splitList(form.paths),
          allowed_ips: splitList(form.ips),
          ...(Number.isNaN(ttl) || ttl < 0 ? {} : { ttl_seconds: ttl }),
          ...(Number.isNaN(maxRps) || maxRps < 0 ? {} : { max_rps: maxRps }),
          ...(Number.isNaN(dailyBytes) || dailyBytes < 0 ? {} : { daily_max_bytes: dailyBytes }),
          allow_public: form.allowPublic,
        })
        onSaved()
        toast.success(`Token "${editing.name}" updated`)
      } else {
        if (!form.name.trim()) {
          setError('Token name is required')
          return
        }
        const created = await api.createToken({
          name: form.name.trim(),
          hostnames: splitList(form.hostnames),
          paths: splitList(form.paths),
          allowed_ips: splitList(form.ips),
          ...(Number.isNaN(ttl) || ttl <= 0 ? {} : { ttl_seconds: ttl }),
          ...(Number.isNaN(maxRps) || maxRps <= 0 ? {} : { max_rps: maxRps }),
          ...(Number.isNaN(dailyBytes) || dailyBytes <= 0 ? {} : { daily_max_bytes: dailyBytes }),
          allow_public: form.allowPublic,
        })
        onSaved()
        onCreated(created.token)
        toast.success(`Token "${form.name.trim()}" created`)
      }
      setOpen(false)
    } catch (e) {
      setError(e instanceof ApiError ? e.message : String(e))
    } finally {
      setBusy(false)
    }
  }

  // Only the string-valued fields use this text helper; allowPublic has its
  // own checkbox below.
  type TextKey = {
    [K in keyof TokenFormState]: TokenFormState[K] extends string ? K : never
  }[keyof TokenFormState]
  const field = (label: string, key: TextKey, placeholder: string) => (
    <div className="grid gap-2">
      <Label htmlFor={`tok-${key}`}>{label}</Label>
      <Input id={`tok-${key}`} value={form[key]} onChange={set(key)} placeholder={placeholder} />
    </div>
  )

  return (
    <Dialog open={open} onOpenChange={openDialog}>
      <DialogTrigger
        render={
          editing ? <Button size="xs" variant="outline" /> : <Button size="sm" />
        }
      >
        {editing ? (
          <>
            <PencilIcon /> Edit
          </>
        ) : (
          <>
            <PlusIcon /> Create Token
          </>
        )}
      </DialogTrigger>
      <DialogContent className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>{editing ? `Edit token "${editing.name}"` : 'Create API token'}</DialogTitle>
          <DialogDescription>
            {editing
              ? 'Adjusts the token scope in place; the secret never changes.'
              : 'Creates a dynamic tunnel token with a restricted scope.'}
          </DialogDescription>
        </DialogHeader>
        <div className="grid gap-4">
          {!editing && field('Name', 'name', 'staging deploys')}
          {field('Allowed hostnames (comma separated, * = all)', 'hostnames', '*')}
          {field('Allowed path binds (comma separated, * = all)', 'paths', '*')}
          {field('Allowed source IPs / CIDRs', 'ips', '0.0.0.0/0')}
          {field(
            editing
              ? 'New lifetime in seconds from now (0 = never, empty = keep)'
              : 'Lifetime in seconds (empty = never expires)',
            'ttl',
            '',
          )}
          {field(
            editing
              ? 'Rate limit (req/s, 0 = no limit, empty = keep)'
              : 'Rate limit (req/s, empty = no limit)',
            'maxRps',
            '',
          )}
          {field(
            editing
              ? 'Daily traffic quota (MB, 0 = no quota, empty = keep)'
              : 'Daily traffic quota (MB, empty = no quota)',
            'dailyMaxMb',
            '',
          )}
          <label className="flex items-center gap-2 text-sm">
            <Checkbox
              checked={form.allowPublic}
              onCheckedChange={(v) => setForm((f) => ({ ...f, allowPublic: v === true }))}
            />
            May publish public services (visitor auth gate skipped)
          </label>
          {error && <p className="text-sm text-destructive">{error}</p>}
        </div>
        <DialogFooter>
          <Button variant="outline" onClick={() => setOpen(false)}>
            Cancel
          </Button>
          <Button onClick={submit} disabled={busy}>
            {busy && <Spinner />} {editing ? 'Save' : 'Create'}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

// Shows the freshly created secret exactly once, with a copy button.
function CreatedTokenDialog({ secret, onClose }: { secret: string | null; onClose: () => void }) {
  return (
    <Dialog
      open={secret !== null}
      onOpenChange={(open) => {
        if (!open) onClose()
      }}
    >
      <DialogContent className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>Token created</DialogTitle>
          <DialogDescription>Copy it now — it will NOT be shown again.</DialogDescription>
        </DialogHeader>
        <div className="flex items-center gap-3">
          <code className="min-w-0 flex-1 break-all rounded-2xl bg-muted px-3 py-2 font-mono text-sm">
            {secret}
          </code>
          <CopyButton value={secret ?? ''} size="sm" />
        </div>
        <DialogFooter>
          <Button variant="outline" onClick={onClose}>
            Close
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

function RevokeButton({ token, onDone }: { token: TokenView; onDone: () => void }) {
  const [busy, setBusy] = useState(false)

  const revoke = async () => {
    setBusy(true)
    try {
      await api.revokeToken(token.id)
      toast.info(`Token "${token.name}" revoked`)
      onDone()
    } catch {
      toast.error(`Could not revoke token "${token.name}"`)
    } finally {
      setBusy(false)
    }
  }

  return (
    <AlertDialog>
      <AlertDialogTrigger render={<Button size="xs" variant="destructive" disabled={busy} />}>
        <Trash2Icon /> Revoke
      </AlertDialogTrigger>
      <AlertDialogContent>
        <AlertDialogHeader>
          <AlertDialogTitle>
            Revoke token "{token.name}" ({token.token_prefix}…)?
          </AlertDialogTitle>
          <AlertDialogDescription>
            New connections with this token will be rejected.
          </AlertDialogDescription>
        </AlertDialogHeader>
        <AlertDialogFooter>
          <AlertDialogCancel>Cancel</AlertDialogCancel>
          <AlertDialogAction
            className="bg-destructive/10 text-destructive hover:bg-destructive/20"
            onClick={() => void revoke()}
          >
            Revoke
          </AlertDialogAction>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  )
}

export function TokensSection() {
  const { data: tokens, refresh } = usePoll(api.tokens, 10_000)
  const [createdSecret, setCreatedSecret] = useState<string | null>(null)

  return (
    <section className="flex flex-col gap-3">
      <SectionHeader title="API Tokens">
        <TokenFormDialog editing={null} onSaved={refresh} onCreated={setCreatedSecret} />
      </SectionHeader>
      <Card className="overflow-hidden py-0">
        <Table>
          <TableHeader>
            <TableRow>
              <TableHead>Name</TableHead>
              <TableHead>Prefix</TableHead>
              <TableHead>Hostnames</TableHead>
              <TableHead>Paths</TableHead>
              <TableHead>Allowed IPs</TableHead>
              <TableHead>Limits</TableHead>
              <TableHead>Expires</TableHead>
              <TableHead className="text-right">Actions</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {tokens === null ? (
              <SkeletonRows rows={4} cols={8} />
            ) : tokens.length === 0 ? (
              <EmptyRow colSpan={8}>No dynamic tokens created</EmptyRow>
            ) : (
              tokens.map((t) => (
                <TableRow key={t.id}>
                  <TableCell className="font-medium">{t.name}</TableCell>
                  <TableCell>
                    <code className="font-mono text-xs">{t.token_prefix}…</code>
                  </TableCell>
                  <TableCell>
                    <BadgeList items={t.hostnames} fallback="*" tint="lime" />
                  </TableCell>
                  <TableCell>
                    <BadgeList items={t.paths} fallback="*" tint="lime" />
                  </TableCell>
                  <TableCell>
                    <BadgeList items={t.allowed_ips} fallback="0.0.0.0/0" tint="gray" />
                  </TableCell>
                  <TableCell>
                    <div className="flex flex-wrap gap-1">
                      {t.max_rps != null && <TintBadge tint="amber">{t.max_rps} req/s</TintBadge>}
                      {t.daily_max_bytes != null && (
                        <TintBadge tint="amber">
                          {Math.round(t.daily_max_bytes / (1024 * 1024))} MB/day
                        </TintBadge>
                      )}
                      {t.allow_public && <TintBadge tint="green">public ok</TintBadge>}
                      {t.max_rps == null && t.daily_max_bytes == null && !t.allow_public && (
                        <span className="text-muted-foreground">—</span>
                      )}
                    </div>
                  </TableCell>
                  <TableCell>
                    <span className={cn('text-sm', t.expired && 'text-destructive')}>
                      {formatExpiry(t.expires_at, t.expired)}
                    </span>
                  </TableCell>
                  <TableCell>
                    <div className="flex justify-end gap-2">
                      <TokenFormDialog editing={t} onSaved={refresh} onCreated={setCreatedSecret} />
                      <RevokeButton token={t} onDone={refresh} />
                    </div>
                  </TableCell>
                </TableRow>
              ))
            )}
          </TableBody>
        </Table>
      </Card>
      <CreatedTokenDialog secret={createdSecret} onClose={() => setCreatedSecret(null)} />
    </section>
  )
}
