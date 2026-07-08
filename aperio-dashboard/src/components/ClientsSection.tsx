import { DrawingPinIcon, MagnifyingGlassIcon, MixerHorizontalIcon } from '@radix-ui/react-icons'
import {
  AlertDialog,
  Badge,
  Button,
  Callout,
  Dialog,
  Flex,
  Heading,
  Skeleton,
  Table,
  Text,
  TextField,
  Tooltip,
} from '@radix-ui/themes'
import { useState, type ReactNode } from 'react'
import { useToast } from '../hooks/useToast'
import { api, ApiError, type ClientDetail } from '../lib/api'
import { formatBandwidth, formatLastPing, formatUptime } from '../lib/format'
import { AddClientWizard } from './AddClientWizard'

// Renders hostname binds; a temporary dashboard override replaces the whole
// set and is shown highlighted with the client-reported values struck through.
function BindList({ binds, override }: { binds: string[]; override: string | null }) {
  if (override) {
    return (
      <Flex gap="1" align="center" wrap="wrap">
        {binds.length > 0 && (
          <Text size="1" color="gray" style={{ textDecoration: 'line-through' }}>
            {binds.join(', ')}
          </Text>
        )}
        <Tooltip content="Temporary override (not persisted)">
          <Badge color="amber">{override}</Badge>
        </Tooltip>
      </Flex>
    )
  }
  if (binds.length === 0) return <Text color="gray">—</Text>
  return (
    <Flex gap="1" wrap="wrap">
      {binds.map((b) => (
        <Badge key={b} color="indigo">
          {b}
        </Badge>
      ))}
    </Flex>
  )
}

// Dialog replacing the old prompt()-based overrule flow: sets or clears the
// temporary hostname/path binds of a connected client.
function OverruleDialog({ client, onDone }: { client: ClientDetail; onDone: () => void }) {
  const [open, setOpen] = useState(false)
  const [hostname, setHostname] = useState('')
  const [path, setPath] = useState('')
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)
  const toast = useToast()
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
      toast(
        `${cleared ? 'Override cleared' : 'Override applied'} for ${client.id.slice(0, 8)}`,
        'green',
      )
      onDone()
    } catch (e) {
      setError(e instanceof ApiError ? e.message : String(e))
    } finally {
      setBusy(false)
    }
  }

  return (
    <Dialog.Root open={open} onOpenChange={openDialog}>
      <Dialog.Trigger>
        <Button size="1" variant="soft">
          <MixerHorizontalIcon /> {hasOverride ? 'Edit' : 'Overrule'}
        </Button>
      </Dialog.Trigger>
      <Dialog.Content maxWidth="440px">
        <Dialog.Title>Overrule client {client.id.slice(0, 8)}…</Dialog.Title>
        <Dialog.Description size="2" color="gray">
          Temporary binds for this connection. Empty fields clear the override; nothing is
          persisted across reconnects.
        </Dialog.Description>
        <Flex direction="column" gap="3" mt="4">
          <label>
            <Text as="div" size="1" weight="medium" color="gray" mb="1">
              HOSTNAME BIND
            </Text>
            <TextField.Root
              value={hostname}
              onChange={(e) => setHostname(e.target.value)}
              placeholder="app.example.com"
            />
          </label>
          <label>
            <Text as="div" size="1" weight="medium" color="gray" mb="1">
              PATH BIND
            </Text>
            <TextField.Root
              value={path}
              onChange={(e) => setPath(e.target.value)}
              placeholder="/api"
            />
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
            Apply
          </Button>
        </Flex>
      </Dialog.Content>
    </Dialog.Root>
  )
}

// Kill switch: a disabled client stays connected but receives no new
// requests; in-flight requests complete. Disabling asks for confirmation.
function EnableToggle({ client, onDone }: { client: ClientDetail; onDone: () => void }) {
  const [busy, setBusy] = useState(false)
  const toast = useToast()

  const setEnabled = async (enabled: boolean) => {
    setBusy(true)
    try {
      await api.setClientEnabled(client.id, enabled)
      toast(
        `Client ${client.id.slice(0, 8)} ${enabled ? 'enabled' : 'disabled'}`,
        enabled ? 'green' : 'gray',
      )
      onDone()
    } catch {
      toast(`Could not update client ${client.id.slice(0, 8)}`, 'red')
    } finally {
      setBusy(false)
    }
  }

  if (client.draining) {
    return (
      <Tooltip content="Client is gracefully shutting down">
        <Button size="1" variant="soft" color="gray" disabled>
          Draining…
        </Button>
      </Tooltip>
    )
  }

  if (!client.enabled) {
    return (
      <Button size="1" variant="soft" color="green" loading={busy} onClick={() => setEnabled(true)}>
        Enable
      </Button>
    )
  }

  return (
    <AlertDialog.Root>
      <AlertDialog.Trigger>
        <Button size="1" variant="soft" color="red" loading={busy}>
          Disable
        </Button>
      </AlertDialog.Trigger>
      <AlertDialog.Content maxWidth="440px">
        <AlertDialog.Title>Disable client {client.id.slice(0, 8)}…?</AlertDialog.Title>
        <AlertDialog.Description size="2">
          It stays connected but receives no new requests; in-flight requests complete.
        </AlertDialog.Description>
        <Flex gap="3" mt="4" justify="end">
          <AlertDialog.Cancel>
            <Button variant="soft" color="gray">
              Cancel
            </Button>
          </AlertDialog.Cancel>
          <AlertDialog.Action>
            <Button color="red" onClick={() => setEnabled(false)}>
              Disable
            </Button>
          </AlertDialog.Action>
        </Flex>
      </AlertDialog.Content>
    </AlertDialog.Root>
  )
}

function EmptyRow({
  colSpan,
  icon,
  children,
}: {
  colSpan: number
  icon?: ReactNode
  children: ReactNode
}) {
  return (
    <Table.Row>
      <Table.Cell colSpan={colSpan}>
        <Flex direction="column" align="center" justify="center" gap="2" p="6">
          {icon && (
            <Text color="gray" size="6" style={{ display: 'inline-flex', opacity: 0.6 }}>
              {icon}
            </Text>
          )}
          <Text color="gray">{children}</Text>
        </Flex>
      </Table.Cell>
    </Table.Row>
  )
}

// Placeholder shimmer rows shown while a table's first fetch is in flight, so
// an empty grid doesn't read as "no data" before the data has arrived.
function SkeletonRows({ rows, cols }: { rows: number; cols: number }) {
  return (
    <>
      {Array.from({ length: rows }).map((_, r) => (
        <Table.Row key={r}>
          {Array.from({ length: cols }).map((_, c) => (
            <Table.Cell key={c}>
              <Skeleton>
                <Text size="2">placeholder</Text>
              </Skeleton>
            </Table.Cell>
          ))}
        </Table.Row>
      ))}
    </>
  )
}

type SortKey = 'requests' | 'connected' | 'ping'

function sortValue(c: ClientDetail, key: SortKey): number {
  if (key === 'requests') return c.request_count
  if (key === 'connected') return c.connected_for_seconds
  return c.last_ping_seconds_ago ?? Number.POSITIVE_INFINITY
}

// A clickable column header that drives the table sort and shows the direction.
function SortHeader({
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
    <Table.ColumnHeaderCell>
      <button
        type="button"
        onClick={() => onSort(sortKey)}
        style={{
          background: 'none',
          border: 'none',
          padding: 0,
          font: 'inherit',
          color: active ? 'var(--accent-11)' : 'inherit',
          cursor: 'pointer',
        }}
      >
        {label}
        {active ? (sort.dir < 0 ? ' ↓' : ' ↑') : ''}
      </button>
    </Table.ColumnHeaderCell>
  )
}

// Kill switch for every currently listed client at once, behind a confirm.
function BulkDisableButton({ count, onConfirm }: { count: number; onConfirm: () => void }) {
  return (
    <AlertDialog.Root>
      <AlertDialog.Trigger>
        <Button size="1" variant="soft" color="red">
          Disable all
        </Button>
      </AlertDialog.Trigger>
      <AlertDialog.Content maxWidth="440px">
        <AlertDialog.Title>Disable {count} client(s)?</AlertDialog.Title>
        <AlertDialog.Description size="2">
          Each stays connected but receives no new requests; in-flight requests complete. Affects
          the clients currently listed (matching your search).
        </AlertDialog.Description>
        <Flex gap="3" mt="4" justify="end">
          <AlertDialog.Cancel>
            <Button variant="soft" color="gray">
              Cancel
            </Button>
          </AlertDialog.Cancel>
          <AlertDialog.Action>
            <Button color="red" onClick={onConfirm}>
              Disable all
            </Button>
          </AlertDialog.Action>
        </Flex>
      </AlertDialog.Content>
    </AlertDialog.Root>
  )
}

export function ClientsSection({
  clients,
  onChanged,
}: {
  clients: ClientDetail[]
  onChanged: () => void
}) {
  const toast = useToast()
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
  const sorted = [...filtered].sort((a, b) => (sortValue(a, sort.key) - sortValue(b, sort.key)) * sort.dir)

  const bulkSet = async (enabled: boolean) => {
    const targets = filtered.filter((c) => c.enabled !== enabled && !c.draining)
    if (targets.length === 0) {
      toast(`No clients to ${enabled ? 'enable' : 'disable'}`, 'gray')
      return
    }
    await Promise.allSettled(targets.map((c) => api.setClientEnabled(c.id, enabled)))
    toast(`${targets.length} client(s) ${enabled ? 'enabled' : 'disabled'}`, enabled ? 'green' : 'gray')
    onChanged()
  }

  return (
    <Flex direction="column" gap="3">
      <Flex justify="between" align="center" gap="3" wrap="wrap">
        <Heading size="4">Active Tunnel Connections</Heading>
        <Flex align="center" gap="2" wrap="wrap">
          <TextField.Root
            placeholder="Search clients…"
            value={search}
            onChange={(e) => setSearch(e.target.value)}
            style={{ width: 220 }}
          >
            <TextField.Slot>
              <MagnifyingGlassIcon />
            </TextField.Slot>
          </TextField.Root>
          {clients.length > 1 && (
            <>
              <BulkDisableButton count={filtered.length} onConfirm={() => void bulkSet(false)} />
              <Button size="1" variant="soft" color="green" onClick={() => void bulkSet(true)}>
                Enable all
              </Button>
            </>
          )}
          <AddClientWizard />
        </Flex>
      </Flex>
      <Table.Root variant="surface">
        <Table.Header>
          <Table.Row>
            <Table.ColumnHeaderCell>Client ID</Table.ColumnHeaderCell>
            <Table.ColumnHeaderCell>IP Address</Table.ColumnHeaderCell>
            <Table.ColumnHeaderCell>Hostname</Table.ColumnHeaderCell>
            <Table.ColumnHeaderCell>Path</Table.ColumnHeaderCell>
            <SortHeader label="Last Ping" sortKey="ping" sort={sort} onSort={onSort} />
            <SortHeader label="Connected For" sortKey="connected" sort={sort} onSort={onSort} />
            <SortHeader label="Requests" sortKey="requests" sort={sort} onSort={onSort} />
            <Table.ColumnHeaderCell>Actions</Table.ColumnHeaderCell>
          </Table.Row>
        </Table.Header>
        <Table.Body>
          {clients.length === 0 ? (
            <EmptyRow colSpan={8} icon={<DrawingPinIcon />}>
              No active client sessions — start a tunnel client to see it here
            </EmptyRow>
          ) : sorted.length === 0 ? (
            <EmptyRow colSpan={8} icon={<MagnifyingGlassIcon />}>
              No clients match “{search}”
            </EmptyRow>
          ) : (
            sorted.map((c) => (
              <Table.Row key={c.id}>
                <Table.Cell>
                  <Flex direction="column">
                    <Flex align="center" gap="1">
                      <Tooltip
                        content={
                          <Flex direction="column" gap="1">
                            {c.instance_id && <Text size="1">client id: {c.instance_id}</Text>}
                            <Text size="1">connection: {c.id}</Text>
                            <Text size="1">
                              {c.token_name ? `token: ${c.token_name}` : 'master token'}
                            </Text>
                          </Flex>
                        }
                      >
                        <Text
                          size="2"
                          style={{ fontFamily: 'var(--code-font-family)', cursor: 'default' }}
                        >
                          {(c.instance_id ?? c.id).slice(0, 8)}…
                        </Text>
                      </Tooltip>
                      {c.service && (
                        <Tooltip content="Service name announced by the client (services: list)">
                          <Badge color="blue" size="1">
                            {c.service}
                          </Badge>
                        </Tooltip>
                      )}
                      {c.public && (
                        <Tooltip content="This client serves its traffic without the visitor auth gate">
                          <Badge color="green" size="1">
                            public
                          </Badge>
                        </Tooltip>
                      )}
                      {c.version && (
                        <Text size="1" color="gray">
                          v{c.version}
                        </Text>
                      )}
                      {c.bandwidth_bps !== null && (
                        <Tooltip content="Announced link capacity; the server paces frames to this client accordingly">
                          <Badge color="gray" size="1">
                            {formatBandwidth(c.bandwidth_bps)}
                          </Badge>
                        </Tooltip>
                      )}
                      {c.priority > 0 && (
                        <Tooltip content={`Standby tier ${c.priority}: receives traffic only when no lower tier is available (primary-standby strategy)`}>
                          <Badge color="gray" size="1">
                            standby {c.priority}
                          </Badge>
                        </Tooltip>
                      )}
                      {c.protocol_mismatch && (
                        <Tooltip
                          content={`Client speaks tunnel protocol v${c.protocol}, server differs — update the older side`}
                        >
                          <Badge color="red" size="1">
                            proto v{c.protocol}
                          </Badge>
                        </Tooltip>
                      )}
                      {c.instance_id_shared && (
                        <Tooltip
                          content={`Another live connection reports the same client id (${c.instance_id}) — bind-tunnels and failover lookups by this id are ambiguous; give each client its own --client-id`}
                        >
                          <Badge color="amber" size="1">
                            SHARED ID
                          </Badge>
                        </Tooltip>
                      )}
                    </Flex>
                    {c.token_name && (
                      <Text size="1" color="gray">
                        🔑 {c.token_name}
                      </Text>
                    )}
                  </Flex>
                </Table.Cell>
                <Table.Cell>
                  <Text size="2" style={{ fontFamily: 'var(--code-font-family)' }}>
                    {c.ip}
                  </Text>
                </Table.Cell>
                <Table.Cell>
                  <BindList binds={c.hostname_binds} override={c.override_hostname_bind} />
                </Table.Cell>
                <Table.Cell>
                  <BindList binds={c.path_bind ? [c.path_bind] : []} override={c.override_path_bind} />
                </Table.Cell>
                <Table.Cell>
                  <Flex align="center" gap="2">
                    <span
                      className={`status-dot ${c.healthy && c.backend_healthy ? 'active' : 'inactive'}`}
                    />
                    <Text size="2">{formatLastPing(c.last_ping_seconds_ago)}</Text>
                    {!c.healthy && (
                      <Badge color="red" size="1">
                        DOWN
                      </Badge>
                    )}
                    {c.healthy && !c.backend_healthy && (
                      <Tooltip content="The client's own health probe reports its backend as down; excluded from routing while the tunnel stays connected">
                        <Badge color="amber" size="1">
                          BACKEND DOWN
                        </Badge>
                      </Tooltip>
                    )}
                  </Flex>
                </Table.Cell>
                <Table.Cell>{formatUptime(c.connected_for_seconds)}</Table.Cell>
                <Table.Cell>{c.request_count}</Table.Cell>
                <Table.Cell>
                  <Flex gap="2">
                    <OverruleDialog client={c} onDone={onChanged} />
                    <EnableToggle client={c} onDone={onChanged} />
                  </Flex>
                </Table.Cell>
              </Table.Row>
            ))
          )}
        </Table.Body>
      </Table.Root>
    </Flex>
  )
}

export { EmptyRow, SkeletonRows }
