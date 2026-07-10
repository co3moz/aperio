import { ArrowDownIcon, ArrowUpIcon, PinIcon, SearchIcon, SlidersHorizontalIcon } from 'lucide-react'
import { useState } from 'react'
import { toast } from 'sonner'
import { AddClientWizard } from './AddClientWizard'
import { EmptyRow, SectionHeader, StatusDot } from './shared'
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
import { Tooltip, TooltipContent, TooltipTrigger } from '@/components/ui/tooltip'
import { api, ApiError, type ClientDetail } from '@/lib/api'
import { formatBandwidth, formatLastPing, formatUptime } from '@/lib/format'
import { useI18n } from '@/i18n'

// Renders hostname binds; a temporary dashboard override replaces the whole
// set and is shown highlighted with the client-reported values struck through.
function BindList({ binds, override }: { binds: string[]; override: string | null }) {
  const { t } = useI18n()
  if (override) {
    return (
      <div className="flex flex-wrap items-center gap-1">
        {binds.length > 0 && (
          <span className="text-xs text-muted-foreground line-through">{binds.join(', ')}</span>
        )}
        <Tooltip>
          <TooltipTrigger render={<span />}>
            <TintBadge tint="amber">{override}</TintBadge>
          </TooltipTrigger>
          <TooltipContent>{t('Temporary override (not persisted)')}</TooltipContent>
        </Tooltip>
      </div>
    )
  }
  if (binds.length === 0) return <span className="text-muted-foreground">—</span>
  return (
    <div className="flex flex-wrap gap-1">
      {binds.map((b) => (
        <TintBadge key={b} tint="lime">
          {b}
        </TintBadge>
      ))}
    </div>
  )
}

/** Small info badge with an explanatory tooltip. */
function HintBadge({
  tint,
  hint,
  children,
}: {
  tint: Parameters<typeof TintBadge>[0]['tint']
  hint: string
  children: React.ReactNode
}) {
  return (
    <Tooltip>
      <TooltipTrigger render={<span />}>
        <TintBadge tint={tint}>{children}</TintBadge>
      </TooltipTrigger>
      <TooltipContent className="max-w-72">{hint}</TooltipContent>
    </Tooltip>
  )
}

// Dialog for the overrule flow: sets or clears the temporary hostname/path
// binds of a connected client.
function OverruleDialog({ client, onDone }: { client: ClientDetail; onDone: () => void }) {
  const { t } = useI18n()
  const [open, setOpen] = useState(false)
  const [hostname, setHostname] = useState('')
  const [path, setPath] = useState('')
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)
  const hasOverride = Boolean(client.override_hostname_bind || client.override_path_bind)

  const openDialog = (next: boolean) => {
    if (next) {
      setHostname(client.override_hostname_bind ?? client.hostname_binds[0] ?? '')
      setPath(client.override_path_bind ?? client.path_bind ?? '')
      setError(null)
    }
    setOpen(next)
  }

  const submit = async () => {
    setBusy(true)
    setError(null)
    try {
      await api.overrideClient(client.id, hostname.trim(), path.trim())
      setOpen(false)
      const cleared = !hostname.trim() && !path.trim()
      toast.success(
        cleared
          ? t('Override cleared for {id}', { id: client.id.slice(0, 8) })
          : t('Override applied for {id}', { id: client.id.slice(0, 8) }),
      )
      onDone()
    } catch (e) {
      setError(e instanceof ApiError ? e.message : String(e))
    } finally {
      setBusy(false)
    }
  }

  return (
    <Dialog open={open} onOpenChange={openDialog}>
      <DialogTrigger render={<Button size="xs" variant="outline" />}>
        <SlidersHorizontalIcon /> {hasOverride ? t('Edit') : t('Overrule')}
      </DialogTrigger>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{t('Overrule client {id}…', { id: client.id.slice(0, 8) })}</DialogTitle>
          <DialogDescription>
            {t('Temporary binds for this connection. Empty fields clear the override; nothing is persisted across reconnects.')}
          </DialogDescription>
        </DialogHeader>
        <div className="grid gap-4">
          <div className="grid gap-2">
            <Label htmlFor={`ovr-host-${client.id}`}>{t('Hostname bind')}</Label>
            <Input
              id={`ovr-host-${client.id}`}
              value={hostname}
              onChange={(e) => setHostname(e.target.value)}
              placeholder="app.example.com"
            />
          </div>
          <div className="grid gap-2">
            <Label htmlFor={`ovr-path-${client.id}`}>{t('Path bind')}</Label>
            <Input
              id={`ovr-path-${client.id}`}
              value={path}
              onChange={(e) => setPath(e.target.value)}
              placeholder="/api"
            />
          </div>
          {error && <p className="text-sm text-destructive">{error}</p>}
        </div>
        <DialogFooter>
          <Button variant="outline" onClick={() => setOpen(false)}>
            {t('Cancel')}
          </Button>
          <Button onClick={submit} disabled={busy}>
            {busy && <Spinner />} {t('Apply')}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

// Kill switch: a disabled client stays connected but receives no new
// requests; in-flight requests complete. Disabling asks for confirmation.
function EnableToggle({ client, onDone }: { client: ClientDetail; onDone: () => void }) {
  const { t } = useI18n()
  const [busy, setBusy] = useState(false)

  const setEnabled = async (enabled: boolean) => {
    setBusy(true)
    try {
      await api.setClientEnabled(client.id, enabled)
      const label = enabled
        ? t('Client {id} enabled', { id: client.id.slice(0, 8) })
        : t('Client {id} disabled', { id: client.id.slice(0, 8) })
      if (enabled) toast.success(label)
      else toast.info(label)
      onDone()
    } catch {
      toast.error(t('Could not update client {id}', { id: client.id.slice(0, 8) }))
    } finally {
      setBusy(false)
    }
  }

  if (client.draining) {
    return (
      <Tooltip>
        <TooltipTrigger render={<span />}>
          <Button size="xs" variant="outline" disabled>
            {t('Draining…')}
          </Button>
        </TooltipTrigger>
        <TooltipContent>{t('Client is gracefully shutting down')}</TooltipContent>
      </Tooltip>
    )
  }

  if (!client.enabled) {
    return (
      <Button size="xs" variant="outline" disabled={busy} onClick={() => void setEnabled(true)}>
        {busy && <Spinner />} {t('Enable')}
      </Button>
    )
  }

  return (
    <AlertDialog>
      <AlertDialogTrigger render={<Button size="xs" variant="destructive" disabled={busy} />}>
        {t('Disable')}
      </AlertDialogTrigger>
      <AlertDialogContent>
        <AlertDialogHeader>
          <AlertDialogTitle>{t('Disable client {id}…?', { id: client.id.slice(0, 8) })}</AlertDialogTitle>
          <AlertDialogDescription>
            {t('It stays connected but receives no new requests; in-flight requests complete.')}
          </AlertDialogDescription>
        </AlertDialogHeader>
        <AlertDialogFooter>
          <AlertDialogCancel>{t('Cancel')}</AlertDialogCancel>
          <AlertDialogAction
            className="bg-destructive/10 text-destructive hover:bg-destructive/20"
            onClick={() => void setEnabled(false)}
          >
            {t('Disable')}
          </AlertDialogAction>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  )
}

type SortKey = 'requests' | 'connected' | 'ping'

function sortValue(c: ClientDetail, key: SortKey): number {
  if (key === 'requests') return c.request_count
  if (key === 'connected') return c.connected_for_seconds
  return c.last_ping_seconds_ago ?? Number.POSITIVE_INFINITY
}

// A clickable column header that drives the table sort and shows the direction.
function SortHead({
  label,
  sortKey,
  sort,
  onSort,
}: {
  label: string
  sortKey: SortKey
  sort: { key: SortKey; dir: 1 | -1 }
  onSort: (key: SortKey) => void
}) {
  const active = sort.key === sortKey
  return (
    <TableHead>
      <button
        type="button"
        onClick={() => onSort(sortKey)}
        className={`inline-flex items-center gap-1 ${active ? 'text-primary' : ''}`}
      >
        {label}
        {active && (sort.dir < 0 ? <ArrowDownIcon className="size-3" /> : <ArrowUpIcon className="size-3" />)}
      </button>
    </TableHead>
  )
}

// Kill switch for every currently listed client at once, behind a confirm.
function BulkDisableButton({ count, onConfirm }: { count: number; onConfirm: () => void }) {
  const { t } = useI18n()
  return (
    <AlertDialog>
      <AlertDialogTrigger render={<Button size="sm" variant="destructive" />}>
        {t('Disable all')}
      </AlertDialogTrigger>
      <AlertDialogContent>
        <AlertDialogHeader>
          <AlertDialogTitle>{t('Disable {count} client(s)?', { count })}</AlertDialogTitle>
          <AlertDialogDescription>
            {t('Each stays connected but receives no new requests; in-flight requests complete. Affects the clients currently listed (matching your search).')}
          </AlertDialogDescription>
        </AlertDialogHeader>
        <AlertDialogFooter>
          <AlertDialogCancel>{t('Cancel')}</AlertDialogCancel>
          <AlertDialogAction
            className="bg-destructive/10 text-destructive hover:bg-destructive/20"
            onClick={onConfirm}
          >
            {t('Disable all')}
          </AlertDialogAction>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  )
}

export function ClientsSection({
  clients,
  onChanged,
}: {
  clients: ClientDetail[]
  onChanged: () => void
}) {
  const { t } = useI18n()
  const [search, setSearch] = useState('')
  const [sort, setSort] = useState<{ key: SortKey; dir: 1 | -1 }>({ key: 'connected', dir: -1 })

  const onSort = (key: SortKey) =>
    setSort((s) => (s.key === key ? { key, dir: s.dir === 1 ? -1 : 1 } : { key, dir: -1 }))

  const needle = search.trim().toLowerCase()
  const filtered = clients.filter(
    (c) =>
      !needle ||
      (c.instance_id ?? c.id).toLowerCase().includes(needle) ||
      c.ip.toLowerCase().includes(needle) ||
      (c.service ?? '').toLowerCase().includes(needle) ||
      (c.token_name ?? '').toLowerCase().includes(needle) ||
      (c.path_bind ?? '').toLowerCase().includes(needle) ||
      c.hostname_binds.some((h) => h.toLowerCase().includes(needle)),
  )
  const sorted = [...filtered].sort(
    (a, b) => (sortValue(a, sort.key) - sortValue(b, sort.key)) * sort.dir,
  )

  const bulkSet = async (enabled: boolean) => {
    const targets = filtered.filter((c) => c.enabled !== enabled && !c.draining)
    if (targets.length === 0) {
      toast.info(enabled ? t('No clients to enable') : t('No clients to disable'))
      return
    }
    await Promise.allSettled(targets.map((c) => api.setClientEnabled(c.id, enabled)))
    const label = enabled
      ? t('{count} client(s) enabled', { count: targets.length })
      : t('{count} client(s) disabled', { count: targets.length })
    if (enabled) toast.success(label)
    else toast.info(label)
    onChanged()
  }

  return (
    <section className="flex flex-col gap-3">
      <SectionHeader title={t('Active Tunnel Connections')}>
        <div className="relative">
          <SearchIcon className="pointer-events-none absolute left-2.5 top-1/2 size-4 -translate-y-1/2 text-muted-foreground" />
          <Input
            placeholder={t('Search clients…')}
            value={search}
            onChange={(e) => setSearch(e.target.value)}
            className="w-56 pl-8"
          />
        </div>
        {clients.length > 1 && (
          <>
            <BulkDisableButton count={filtered.length} onConfirm={() => void bulkSet(false)} />
            <Button size="sm" variant="outline" onClick={() => void bulkSet(true)}>
              {t('Enable all')}
            </Button>
          </>
        )}
        <AddClientWizard />
      </SectionHeader>
      <Card className="overflow-hidden py-0">
        <Table>
          <TableHeader>
            <TableRow>
              <TableHead>{t('Client ID')}</TableHead>
              <TableHead>{t('IP Address')}</TableHead>
              <TableHead>{t('Hostname')}</TableHead>
              <TableHead>{t('Path')}</TableHead>
              <SortHead label={t('Last Ping')} sortKey="ping" sort={sort} onSort={onSort} />
              <SortHead label={t('Connected For')} sortKey="connected" sort={sort} onSort={onSort} />
              <SortHead label={t('Requests')} sortKey="requests" sort={sort} onSort={onSort} />
              <TableHead className="text-right">{t('Actions')}</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {clients.length === 0 ? (
              <EmptyRow colSpan={8} icon={<PinIcon />}>
                {t('No active client sessions — start a tunnel client to see it here')}
              </EmptyRow>
            ) : sorted.length === 0 ? (
              <EmptyRow colSpan={8} icon={<SearchIcon />}>
                {t('No clients match "{search}"', { search })}
              </EmptyRow>
            ) : (
              sorted.map((c) => (
                <TableRow key={c.id}>
                  <TableCell>
                    <div className="flex flex-col gap-0.5">
                      <div className="flex flex-wrap items-center gap-1">
                        <Tooltip>
                          <TooltipTrigger
                            render={<span className="cursor-default font-mono text-sm" />}
                          >
                            {(c.instance_id ?? c.id).slice(0, 8)}…
                          </TooltipTrigger>
                          <TooltipContent>
                            <div className="flex flex-col gap-0.5 font-mono text-xs">
                              {c.instance_id && <span>{t('client id')}: {c.instance_id}</span>}
                              <span>{t('connection')}: {c.id}</span>
                              <span>{c.token_name ? `${t('token')}: ${c.token_name}` : t('master token')}</span>
                            </div>
                          </TooltipContent>
                        </Tooltip>
                        {c.service && (
                          <HintBadge tint="blue" hint={t('Service name announced by the client (services: list)')}>
                            {c.service}
                          </HintBadge>
                        )}
                        {c.public && (
                          <HintBadge
                            tint="green"
                            hint={t('This client serves its traffic without the visitor auth gate')}
                          >
                            {t('public')}
                          </HintBadge>
                        )}
                        {c.visitor_auth && (
                          <HintBadge
                            tint="lime"
                            hint={t("This client gates its service behind a client-set visitor login, overriding the server's own visitor password for this service")}
                          >
                            {t('custom auth')}
                          </HintBadge>
                        )}
                        {c.version && (
                          <span className="text-xs text-muted-foreground">v{c.version}</span>
                        )}
                        {c.bandwidth_bps !== null && (
                          <HintBadge
                            tint="gray"
                            hint={t('Announced link capacity; the server paces frames to this client accordingly')}
                          >
                            {formatBandwidth(c.bandwidth_bps)}
                          </HintBadge>
                        )}
                        {c.priority > 0 && (
                          <HintBadge
                            tint="gray"
                            hint={t('Standby tier {tier}: receives traffic only when no lower tier is available (primary-standby strategy)', { tier: c.priority })}
                          >
                            {t('standby')} {c.priority}
                          </HintBadge>
                        )}
                        {c.protocol_mismatch && (
                          <HintBadge
                            tint="red"
                            hint={t('Client speaks tunnel protocol v{proto}, server differs — update the older side', { proto: c.protocol ?? 0 })}
                          >
                            proto v{c.protocol}
                          </HintBadge>
                        )}
                        {c.instance_id_shared && (
                          <HintBadge
                            tint="amber"
                            hint={t('Another live connection reports the same client id ({id}) — bind-tunnels and failover lookups by this id are ambiguous; give each client its own --client-id', { id: c.instance_id ?? '' })}
                          >
                            {t('SHARED ID')}
                          </HintBadge>
                        )}
                      </div>
                      {c.token_name && (
                        <span className="text-xs text-muted-foreground">🔑 {c.token_name}</span>
                      )}
                    </div>
                  </TableCell>
                  <TableCell className="font-mono text-sm">{c.ip}</TableCell>
                  <TableCell>
                    <BindList binds={c.hostname_binds} override={c.override_hostname_bind} />
                  </TableCell>
                  <TableCell>
                    <BindList
                      binds={c.path_bind ? [c.path_bind] : []}
                      override={c.override_path_bind}
                    />
                  </TableCell>
                  <TableCell>
                    <div className="flex items-center gap-2">
                      <StatusDot active={c.healthy && c.backend_healthy} />
                      <span className="text-sm">{formatLastPing(c.last_ping_seconds_ago)}</span>
                      {!c.healthy && <TintBadge tint="red">{t('DOWN')}</TintBadge>}
                      {c.healthy && !c.backend_healthy && (
                        <HintBadge
                          tint="amber"
                          hint={t("The client's own health probe reports its backend as down; excluded from routing while the tunnel stays connected")}
                        >
                          {t('BACKEND DOWN')}
                        </HintBadge>
                      )}
                    </div>
                  </TableCell>
                  <TableCell>{formatUptime(c.connected_for_seconds)}</TableCell>
                  <TableCell className="tabular-nums">{c.request_count}</TableCell>
                  <TableCell>
                    <div className="flex justify-end gap-2">
                      <OverruleDialog client={c} onDone={onChanged} />
                      <EnableToggle client={c} onDone={onChanged} />
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
