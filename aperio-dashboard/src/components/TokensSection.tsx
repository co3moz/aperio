import { CheckIcon, CopyIcon, Pencil1Icon, PlusIcon, TrashIcon } from '@radix-ui/react-icons'
import {
  AlertDialog,
  Badge,
  Button,
  Callout,
  Checkbox,
  Code,
  Dialog,
  Flex,
  Heading,
  Table,
  Text,
  TextField,
} from '@radix-ui/themes'
import { useState } from 'react'
import { usePoll } from '../hooks/usePoll'
import { useToast } from '../hooks/useToast'
import { api, ApiError, type TokenView } from '../lib/api'
import { formatExpiry, splitList } from '../lib/format'
import { EmptyRow, SkeletonRows } from './ClientsSection'

function BadgeList({ items, fallback, color }: { items: string[]; fallback: string; color: 'indigo' | 'gray' }) {
  const shown = items.length ? items : [fallback]
  return (
    <Flex gap="1" wrap="wrap">
      {shown.map((item) => (
        <Badge key={item} color={color}>
          {item}
        </Badge>
      ))}
    </Flex>
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
  const toast = useToast()

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
        toast(`Token "${editing.name}" updated`, 'green')
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
        toast(`Token "${form.name.trim()}" created`, 'green')
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
    <label>
      <Text as="div" size="1" weight="medium" color="gray" mb="1">
        {label}
      </Text>
      <TextField.Root value={form[key]} onChange={set(key)} placeholder={placeholder} />
    </label>
  )

  return (
    <Dialog.Root open={open} onOpenChange={openDialog}>
      <Dialog.Trigger>
        {editing ? (
          <Button size="1" variant="soft">
            <Pencil1Icon /> Edit
          </Button>
        ) : (
          <Button size="2" variant="soft">
            <PlusIcon /> Create Token
          </Button>
        )}
      </Dialog.Trigger>
      <Dialog.Content maxWidth="480px">
        <Dialog.Title>{editing ? `Edit token "${editing.name}"` : 'Create API token'}</Dialog.Title>
        <Dialog.Description size="2" color="gray">
          {editing
            ? 'Adjusts the token scope in place; the secret never changes.'
            : 'Creates a dynamic tunnel token with a restricted scope.'}
        </Dialog.Description>
        <Flex direction="column" gap="3" mt="4">
          {!editing && field('NAME', 'name', 'staging deploys')}
          {field('ALLOWED HOSTNAMES (COMMA SEPARATED, * = ALL)', 'hostnames', '*')}
          {field('ALLOWED PATH BINDS (COMMA SEPARATED, * = ALL)', 'paths', '*')}
          {field('ALLOWED SOURCE IPS / CIDRS', 'ips', '0.0.0.0/0')}
          {field(
            editing
              ? 'NEW LIFETIME IN SECONDS FROM NOW (0 = NEVER, EMPTY = KEEP)'
              : 'LIFETIME IN SECONDS (EMPTY = NEVER EXPIRES)',
            'ttl',
            '',
          )}
          {field(
            editing
              ? 'RATE LIMIT (REQ/S, 0 = NO LIMIT, EMPTY = KEEP)'
              : 'RATE LIMIT (REQ/S, EMPTY = NO LIMIT)',
            'maxRps',
            '',
          )}
          {field(
            editing
              ? 'DAILY TRAFFIC QUOTA (MB, 0 = NO QUOTA, EMPTY = KEEP)'
              : 'DAILY TRAFFIC QUOTA (MB, EMPTY = NO QUOTA)',
            'dailyMaxMb',
            '',
          )}
          <label>
            <Flex align="center" gap="2">
              <Checkbox
                checked={form.allowPublic}
                onCheckedChange={(v) => setForm((f) => ({ ...f, allowPublic: v === true }))}
              />
              <Text size="1" weight="medium" color="gray">
                MAY PUBLISH PUBLIC SERVICES (VISITOR AUTH GATE SKIPPED)
              </Text>
            </Flex>
          </label>
          {error && (
            <Callout.Root color="red" size="1">
              <Callout.Text>{error}</Callout.Text>
            </Callout.Root>
          )}
        </Flex>
        <Flex gap="3" mt="4" justify="end">
          <Dialog.Close>
            <Button variant="soft" color="gray">
              Cancel
            </Button>
          </Dialog.Close>
          <Button onClick={submit} loading={busy}>
            {editing ? 'Save' : 'Create'}
          </Button>
        </Flex>
      </Dialog.Content>
    </Dialog.Root>
  )
}

// Shows the freshly created secret exactly once, with a copy button.
function CreatedTokenDialog({ secret, onClose }: { secret: string | null; onClose: () => void }) {
  const [copied, setCopied] = useState(false)

  const copy = async () => {
    try {
      await navigator.clipboard.writeText(secret ?? '')
      setCopied(true)
    } catch {
      // Clipboard may be unavailable; the secret stays selectable below.
    }
  }

  return (
    <Dialog.Root
      open={secret !== null}
      onOpenChange={(open) => {
        if (!open) {
          setCopied(false)
          onClose()
        }
      }}
    >
      <Dialog.Content maxWidth="480px">
        <Dialog.Title>Token created</Dialog.Title>
        <Dialog.Description size="2" color="gray">
          Copy it now — it will NOT be shown again.
        </Dialog.Description>
        <Flex align="center" gap="3" mt="4">
          <Code size="2" style={{ wordBreak: 'break-all' }}>
            {secret}
          </Code>
          <Button size="1" variant="soft" onClick={copy}>
            {copied ? <CheckIcon /> : <CopyIcon />} {copied ? 'Copied' : 'Copy'}
          </Button>
        </Flex>
        <Flex mt="4" justify="end">
          <Dialog.Close>
            <Button variant="soft" color="gray">
              Close
            </Button>
          </Dialog.Close>
        </Flex>
      </Dialog.Content>
    </Dialog.Root>
  )
}

function RevokeButton({ token, onDone }: { token: TokenView; onDone: () => void }) {
  const [busy, setBusy] = useState(false)
  const toast = useToast()

  const revoke = async () => {
    setBusy(true)
    try {
      await api.revokeToken(token.id)
      toast(`Token "${token.name}" revoked`, 'gray')
      onDone()
    } catch {
      toast(`Could not revoke token "${token.name}"`, 'red')
    } finally {
      setBusy(false)
    }
  }

  return (
    <AlertDialog.Root>
      <AlertDialog.Trigger>
        <Button size="1" variant="soft" color="red" loading={busy}>
          <TrashIcon /> Revoke
        </Button>
      </AlertDialog.Trigger>
      <AlertDialog.Content maxWidth="440px">
        <AlertDialog.Title>
          Revoke token "{token.name}" ({token.token_prefix}…)?
        </AlertDialog.Title>
        <AlertDialog.Description size="2">
          New connections with this token will be rejected.
        </AlertDialog.Description>
        <Flex gap="3" mt="4" justify="end">
          <AlertDialog.Cancel>
            <Button variant="soft" color="gray">
              Cancel
            </Button>
          </AlertDialog.Cancel>
          <AlertDialog.Action>
            <Button color="red" onClick={revoke}>
              Revoke
            </Button>
          </AlertDialog.Action>
        </Flex>
      </AlertDialog.Content>
    </AlertDialog.Root>
  )
}

export function TokensSection() {
  const { data: tokens, refresh } = usePoll(api.tokens, 10_000)
  const [createdSecret, setCreatedSecret] = useState<string | null>(null)

  return (
    <Flex direction="column" gap="3">
      <Flex justify="between" align="center">
        <Heading size="4">API Tokens</Heading>
        <TokenFormDialog editing={null} onSaved={refresh} onCreated={setCreatedSecret} />
      </Flex>
      <Table.Root variant="surface">
        <Table.Header>
          <Table.Row>
            <Table.ColumnHeaderCell>Name</Table.ColumnHeaderCell>
            <Table.ColumnHeaderCell>Prefix</Table.ColumnHeaderCell>
            <Table.ColumnHeaderCell>Hostnames</Table.ColumnHeaderCell>
            <Table.ColumnHeaderCell>Paths</Table.ColumnHeaderCell>
            <Table.ColumnHeaderCell>Allowed IPs</Table.ColumnHeaderCell>
            <Table.ColumnHeaderCell>Limits</Table.ColumnHeaderCell>
            <Table.ColumnHeaderCell>Expires</Table.ColumnHeaderCell>
            <Table.ColumnHeaderCell>Actions</Table.ColumnHeaderCell>
          </Table.Row>
        </Table.Header>
        <Table.Body>
          {tokens === null ? (
            <SkeletonRows rows={4} cols={8} />
          ) : tokens.length === 0 ? (
            <EmptyRow colSpan={8}>No dynamic tokens created</EmptyRow>
          ) : (
            tokens.map((t) => (
              <Table.Row key={t.id}>
                <Table.Cell>{t.name}</Table.Cell>
                <Table.Cell>
                  <Code size="2">{t.token_prefix}…</Code>
                </Table.Cell>
                <Table.Cell>
                  <BadgeList items={t.hostnames} fallback="*" color="indigo" />
                </Table.Cell>
                <Table.Cell>
                  <BadgeList items={t.paths} fallback="*" color="indigo" />
                </Table.Cell>
                <Table.Cell>
                  <BadgeList items={t.allowed_ips} fallback="0.0.0.0/0" color="gray" />
                </Table.Cell>
                <Table.Cell>
                  <Flex gap="1" wrap="wrap">
                    {t.max_rps != null && (
                      <Badge color="orange" size="1">
                        {t.max_rps} req/s
                      </Badge>
                    )}
                    {t.daily_max_bytes != null && (
                      <Badge color="orange" size="1">
                        {Math.round(t.daily_max_bytes / (1024 * 1024))} MB/day
                      </Badge>
                    )}
                    {t.allow_public && (
                      <Badge color="green" size="1">
                        public ok
                      </Badge>
                    )}
                    {t.max_rps == null && t.daily_max_bytes == null && !t.allow_public && (
                      <Text size="2" color="gray">
                        —
                      </Text>
                    )}
                  </Flex>
                </Table.Cell>
                <Table.Cell>
                  <Text size="2" color={t.expired ? 'red' : undefined}>
                    {formatExpiry(t.expires_at, t.expired)}
                  </Text>
                </Table.Cell>
                <Table.Cell>
                  <Flex gap="2">
                    <TokenFormDialog editing={t} onSaved={refresh} onCreated={setCreatedSecret} />
                    <RevokeButton token={t} onDone={refresh} />
                  </Flex>
                </Table.Cell>
              </Table.Row>
            ))
          )}
        </Table.Body>
      </Table.Root>
      <CreatedTokenDialog secret={createdSecret} onClose={() => setCreatedSecret(null)} />
    </Flex>
  )
}
