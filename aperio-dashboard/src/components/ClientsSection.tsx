import { MixerHorizontalIcon } from '@radix-ui/react-icons'
import {
  AlertDialog,
  Badge,
  Button,
  Callout,
  Dialog,
  Flex,
  Heading,
  Table,
  Text,
  TextField,
  Tooltip,
} from '@radix-ui/themes'
import { useState, type ReactNode } from 'react'
import { api, ApiError, type ClientDetail } from '../lib/api'
import { formatLastPing, formatUptime } from '../lib/format'

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

  const setEnabled = async (enabled: boolean) => {
    setBusy(true)
    try {
      await api.setClientEnabled(client.id, enabled)
      onDone()
    } catch {
      // Next stats poll shows the actual state.
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

function EmptyRow({ colSpan, children }: { colSpan: number; children: ReactNode }) {
  return (
    <Table.Row>
      <Table.Cell colSpan={colSpan}>
        <Flex justify="center" p="5">
          <Text color="gray">{children}</Text>
        </Flex>
      </Table.Cell>
    </Table.Row>
  )
}

export function ClientsSection({
  clients,
  onChanged,
}: {
  clients: ClientDetail[]
  onChanged: () => void
}) {
  return (
    <Flex direction="column" gap="3">
      <Heading size="4">Active Tunnel Connections</Heading>
      <Table.Root variant="surface">
        <Table.Header>
          <Table.Row>
            <Table.ColumnHeaderCell>Client ID</Table.ColumnHeaderCell>
            <Table.ColumnHeaderCell>IP Address</Table.ColumnHeaderCell>
            <Table.ColumnHeaderCell>Hostname</Table.ColumnHeaderCell>
            <Table.ColumnHeaderCell>Path</Table.ColumnHeaderCell>
            <Table.ColumnHeaderCell>Last Ping</Table.ColumnHeaderCell>
            <Table.ColumnHeaderCell>Connected For</Table.ColumnHeaderCell>
            <Table.ColumnHeaderCell>Requests</Table.ColumnHeaderCell>
            <Table.ColumnHeaderCell>Actions</Table.ColumnHeaderCell>
          </Table.Row>
        </Table.Header>
        <Table.Body>
          {clients.length === 0 ? (
            <EmptyRow colSpan={8}>No active client sessions</EmptyRow>
          ) : (
            clients.map((c) => (
              <Table.Row key={c.id}>
                <Table.Cell>
                  <Tooltip content={`${c.id} • ${c.token_name ? `token: ${c.token_name}` : 'master token'}`}>
                    <Flex direction="column">
                      <Text size="2" style={{ fontFamily: 'var(--code-font-family)' }}>
                        {c.id.slice(0, 8)}…
                      </Text>
                      {c.token_name && (
                        <Text size="1" color="gray">
                          🔑 {c.token_name}
                        </Text>
                      )}
                    </Flex>
                  </Tooltip>
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
                    <span className={`status-dot ${c.healthy ? 'active' : 'inactive'}`} />
                    <Text size="2">{formatLastPing(c.last_ping_seconds_ago)}</Text>
                    {!c.healthy && (
                      <Badge color="red" size="1">
                        DOWN
                      </Badge>
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

export { EmptyRow }
