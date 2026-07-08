import { PlusIcon, TrashIcon } from '@radix-ui/react-icons'
import {
  AlertDialog,
  Badge,
  Button,
  Callout,
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
import { api, ApiError, type Webhook } from '../lib/api'
import { splitList } from '../lib/format'
import { EmptyRow, SkeletonRows } from './ClientsSection'

const KNOWN_EVENTS =
  'client_connected, client_disconnected, client_draining, token_created, token_revoked, tunnel_created, tunnel_deleted, share_created, maintenance_on, maintenance_off'

function CreateWebhookDialog({ onCreated }: { onCreated: () => void }) {
  const [open, setOpen] = useState(false)
  const [name, setName] = useState('')
  const [url, setUrl] = useState('')
  const [events, setEvents] = useState('*')
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)
  const toast = useToast()

  const openDialog = (next: boolean) => {
    if (next) {
      setName('')
      setUrl('')
      setEvents('*')
      setError(null)
    }
    setOpen(next)
  }

  const submit = async () => {
    setBusy(true)
    setError(null)
    try {
      await api.createWebhook({ name: name.trim(), url: url.trim(), events: splitList(events) })
      setOpen(false)
      toast(`Webhook "${name.trim()}" added`, 'green')
      onCreated()
    } catch (e) {
      setError(e instanceof ApiError ? e.message : String(e))
    } finally {
      setBusy(false)
    }
  }

  return (
    <Dialog.Root open={open} onOpenChange={openDialog}>
      <Dialog.Trigger>
        <Button size="2" variant="soft">
          <PlusIcon /> Add Webhook
        </Button>
      </Dialog.Trigger>
      <Dialog.Content maxWidth="480px">
        <Dialog.Title>Add webhook</Dialog.Title>
        <Dialog.Description size="2" color="gray">
          Known events: {KNOWN_EVENTS}. Use * to subscribe to everything.
        </Dialog.Description>
        <Flex direction="column" gap="3" mt="4">
          <label>
            <Text as="div" size="1" weight="medium" color="gray" mb="1">
              NAME
            </Text>
            <TextField.Root value={name} onChange={(e) => setName(e.target.value)} placeholder="ops-alerts" />
          </label>
          <label>
            <Text as="div" size="1" weight="medium" color="gray" mb="1">
              URL
            </Text>
            <TextField.Root
              value={url}
              onChange={(e) => setUrl(e.target.value)}
              placeholder="https://example.com/hooks/aperio"
            />
          </label>
          <label>
            <Text as="div" size="1" weight="medium" color="gray" mb="1">
              EVENTS (COMMA SEPARATED, * = ALL)
            </Text>
            <TextField.Root value={events} onChange={(e) => setEvents(e.target.value)} placeholder="*" />
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
            Add
          </Button>
        </Flex>
      </Dialog.Content>
    </Dialog.Root>
  )
}

function DeleteWebhookButton({ hook, onDone }: { hook: Webhook; onDone: () => void }) {
  const toast = useToast()
  const remove = async () => {
    try {
      await api.deleteWebhook(hook.id)
      toast(`Webhook "${hook.name}" deleted`, 'gray')
    } finally {
      onDone()
    }
  }

  return (
    <AlertDialog.Root>
      <AlertDialog.Trigger>
        <Button size="1" variant="soft" color="red">
          <TrashIcon /> Delete
        </Button>
      </AlertDialog.Trigger>
      <AlertDialog.Content maxWidth="440px">
        <AlertDialog.Title>Delete webhook "{hook.name}"?</AlertDialog.Title>
        <AlertDialog.Description size="2">
          No further events will be delivered to {hook.url}.
        </AlertDialog.Description>
        <Flex gap="3" mt="4" justify="end">
          <AlertDialog.Cancel>
            <Button variant="soft" color="gray">
              Cancel
            </Button>
          </AlertDialog.Cancel>
          <AlertDialog.Action>
            <Button color="red" onClick={remove}>
              Delete
            </Button>
          </AlertDialog.Action>
        </Flex>
      </AlertDialog.Content>
    </AlertDialog.Root>
  )
}

export function WebhooksSection() {
  const { data: hooks, refresh } = usePoll(api.webhooks, 15_000)

  return (
    <Flex direction="column" gap="3">
      <Flex justify="between" align="center">
        <Heading size="4">Webhooks</Heading>
        <CreateWebhookDialog onCreated={refresh} />
      </Flex>
      <Table.Root variant="surface">
        <Table.Header>
          <Table.Row>
            <Table.ColumnHeaderCell>Name</Table.ColumnHeaderCell>
            <Table.ColumnHeaderCell>URL</Table.ColumnHeaderCell>
            <Table.ColumnHeaderCell>Events</Table.ColumnHeaderCell>
            <Table.ColumnHeaderCell>Actions</Table.ColumnHeaderCell>
          </Table.Row>
        </Table.Header>
        <Table.Body>
          {hooks === null ? (
            <SkeletonRows rows={3} cols={4} />
          ) : hooks.length === 0 ? (
            <EmptyRow colSpan={4}>No webhooks defined</EmptyRow>
          ) : (
            hooks.map((h) => (
              <Table.Row key={h.id}>
                <Table.Cell>{h.name}</Table.Cell>
                <Table.Cell>
                  <Code size="2" style={{ wordBreak: 'break-all' }}>
                    {h.url}
                  </Code>
                </Table.Cell>
                <Table.Cell>
                  <Flex gap="1" wrap="wrap">
                    {(h.events.length ? h.events : ['*']).map((e) => (
                      <Badge key={e} color="indigo">
                        {e}
                      </Badge>
                    ))}
                  </Flex>
                </Table.Cell>
                <Table.Cell>
                  <DeleteWebhookButton hook={h} onDone={refresh} />
                </Table.Cell>
              </Table.Row>
            ))
          )}
        </Table.Body>
      </Table.Root>
    </Flex>
  )
}
