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
import { useI18n } from '@/i18n'
import { useHasRole } from '@/lib/session'

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

function formFromToken(tok: TokenView | null): TokenFormState {
  return {
    name: tok?.name ?? '',
    hostnames: tok ? (tok.hostnames.length ? tok.hostnames : ['*']).join(', ') : '*',
    paths: tok ? (tok.paths.length ? tok.paths : ['*']).join(', ') : '*',
    ips: tok ? (tok.allowed_ips.length ? tok.allowed_ips : ['0.0.0.0/0']).join(', ') : '0.0.0.0/0',
    ttl: '',
    maxRps: tok?.max_rps != null ? String(tok.max_rps) : '',
    dailyMaxMb: tok?.daily_max_bytes != null ? String(tok.daily_max_bytes / (1024 * 1024)) : '',
    allowPublic: tok?.allow_public ?? false,
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
  const { t } = useI18n()
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
        toast.success(t('Token "{name}" updated', { name: editing.name }))
      } else {
        if (!form.name.trim()) {
          setError(t('Token name is required'))
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
        toast.success(t('Token "{name}" created', { name: form.name.trim() }))
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
            <PencilIcon /> {t('Edit')}
          </>
        ) : (
          <>
            <PlusIcon /> {t('Create Token')}
          </>
        )}
      </DialogTrigger>
      <DialogContent className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>{editing ? t('Edit token "{name}"', { name: editing.name }) : t('Create API token')}</DialogTitle>
          <DialogDescription>
            {editing
              ? t('Adjusts the token scope in place; the secret never changes.')
              : t('Creates a dynamic tunnel token with a restricted scope.')}
          </DialogDescription>
        </DialogHeader>
        <div className="grid gap-4">
          {!editing && field(t('Name'), 'name', 'staging deploys')}
          {field(t('Allowed hostnames (comma separated, * = all)'), 'hostnames', '*')}
          {field(t('Allowed path binds (comma separated, * = all)'), 'paths', '*')}
          {field(t('Allowed source IPs / CIDRs'), 'ips', '0.0.0.0/0')}
          {field(
            editing
              ? t('New lifetime in seconds from now (0 = never, empty = keep)')
              : t('Lifetime in seconds (empty = never expires)'),
            'ttl',
            '',
          )}
          {field(
            editing
              ? t('Rate limit (req/s, 0 = no limit, empty = keep)')
              : t('Rate limit (req/s, empty = no limit)'),
            'maxRps',
            '',
          )}
          {field(
            editing
              ? t('Daily traffic quota (MB, 0 = no quota, empty = keep)')
              : t('Daily traffic quota (MB, empty = no quota)'),
            'dailyMaxMb',
            '',
          )}
          <label className="flex items-center gap-2 text-sm">
            <Checkbox
              checked={form.allowPublic}
              onCheckedChange={(v) => setForm((f) => ({ ...f, allowPublic: v === true }))}
            />
            {t('May publish public services (visitor auth gate skipped)')}
          </label>
          {error && <p className="text-sm text-destructive">{error}</p>}
        </div>
        <DialogFooter>
          <Button variant="outline" onClick={() => setOpen(false)}>
            {t('Cancel')}
          </Button>
          <Button onClick={submit} disabled={busy}>
            {busy && <Spinner />} {editing ? t('Save') : t('Create')}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

// Shows the freshly created secret exactly once, with a copy button.
function CreatedTokenDialog({ secret, onClose }: { secret: string | null; onClose: () => void }) {
  const { t } = useI18n()
  return (
    <Dialog
      open={secret !== null}
      onOpenChange={(open) => {
        if (!open) onClose()
      }}
    >
      <DialogContent className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>{t('Token created')}</DialogTitle>
          <DialogDescription>{t('Copy it now — it will NOT be shown again.')}</DialogDescription>
        </DialogHeader>
        <div className="flex items-center gap-3">
          <code className="min-w-0 flex-1 break-all rounded-2xl bg-muted px-3 py-2 font-mono text-sm">
            {secret}
          </code>
          <CopyButton value={secret ?? ''} size="sm" />
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

function RevokeButton({ token, onDone }: { token: TokenView; onDone: () => void }) {
  const { t } = useI18n()
  const [busy, setBusy] = useState(false)

  const revoke = async () => {
    setBusy(true)
    try {
      await api.revokeToken(token.id)
      toast.info(t('Token "{name}" revoked', { name: token.name }))
      onDone()
    } catch {
      toast.error(t('Could not revoke token "{name}"', { name: token.name }))
    } finally {
      setBusy(false)
    }
  }

  return (
    <AlertDialog>
      <AlertDialogTrigger render={<Button size="xs" variant="destructive" disabled={busy} />}>
        <Trash2Icon /> {t('Revoke')}
      </AlertDialogTrigger>
      <AlertDialogContent>
        <AlertDialogHeader>
          <AlertDialogTitle>
            {t('Revoke token "{name}" ({prefix}…)?', { name: token.name, prefix: token.token_prefix })}
          </AlertDialogTitle>
          <AlertDialogDescription>
            {t('New connections with this token will be rejected.')}
          </AlertDialogDescription>
        </AlertDialogHeader>
        <AlertDialogFooter>
          <AlertDialogCancel>{t('Cancel')}</AlertDialogCancel>
          <AlertDialogAction
            className="bg-destructive/10 text-destructive hover:bg-destructive/20"
            onClick={() => void revoke()}
          >
            {t('Revoke')}
          </AlertDialogAction>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  )
}

export function TokensSection() {
  const { t } = useI18n()
  const canMutate = useHasRole('operator')
  const { data: tokens, refresh } = usePoll(api.tokens, 10_000)
  const [createdSecret, setCreatedSecret] = useState<string | null>(null)

  return (
    <section className="flex flex-col gap-3">
      <SectionHeader title={t('API Tokens')}>
        {canMutate && <TokenFormDialog editing={null} onSaved={refresh} onCreated={setCreatedSecret} />}
      </SectionHeader>
      <Card className="overflow-hidden py-0">
        <Table>
          <TableHeader>
            <TableRow>
              <TableHead>{t('Name')}</TableHead>
              <TableHead>{t('Prefix')}</TableHead>
              <TableHead>{t('Hostnames')}</TableHead>
              <TableHead>{t('Paths')}</TableHead>
              <TableHead>{t('Allowed IPs')}</TableHead>
              <TableHead>{t('Limits')}</TableHead>
              <TableHead>{t('Expires')}</TableHead>
              <TableHead className="text-right">{t('Actions')}</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {tokens === null ? (
              <SkeletonRows rows={4} cols={8} />
            ) : tokens.length === 0 ? (
              <EmptyRow colSpan={8}>{t('No dynamic tokens created')}</EmptyRow>
            ) : (
              tokens.map((tok) => (
                <TableRow key={tok.id}>
                  <TableCell className="font-medium">{tok.name}</TableCell>
                  <TableCell>
                    <code className="font-mono text-xs">{tok.token_prefix}…</code>
                  </TableCell>
                  <TableCell>
                    <BadgeList items={tok.hostnames} fallback="*" tint="lime" />
                  </TableCell>
                  <TableCell>
                    <BadgeList items={tok.paths} fallback="*" tint="lime" />
                  </TableCell>
                  <TableCell>
                    <BadgeList items={tok.allowed_ips} fallback="0.0.0.0/0" tint="gray" />
                  </TableCell>
                  <TableCell>
                    <div className="flex flex-wrap gap-1">
                      {tok.max_rps != null && <TintBadge tint="amber">{tok.max_rps} req/s</TintBadge>}
                      {tok.daily_max_bytes != null && (
                        <TintBadge tint="amber">
                          {Math.round(tok.daily_max_bytes / (1024 * 1024))} MB/day
                        </TintBadge>
                      )}
                      {tok.allow_public && <TintBadge tint="green">{t('public ok')}</TintBadge>}
                      {tok.max_rps == null && tok.daily_max_bytes == null && !tok.allow_public && (
                        <span className="text-muted-foreground">—</span>
                      )}
                    </div>
                  </TableCell>
                  <TableCell>
                    <span className={cn('text-sm', tok.expired && 'text-destructive')}>
                      {formatExpiry(tok.expires_at, tok.expired)}
                    </span>
                  </TableCell>
                  <TableCell>
                    <div className="flex justify-end gap-2">
                      {canMutate ? (
                        <>
                          <TokenFormDialog editing={tok} onSaved={refresh} onCreated={setCreatedSecret} />
                          <RevokeButton token={tok} onDone={refresh} />
                        </>
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
      <CreatedTokenDialog secret={createdSecret} onClose={() => setCreatedSecret(null)} />
    </section>
  )
}
