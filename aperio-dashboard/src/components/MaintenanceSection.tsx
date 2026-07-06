import { Cross2Icon, ExclamationTriangleIcon } from '@radix-ui/react-icons'
import { Badge, Button, Callout, Card, Flex, Heading, IconButton, Text, TextField, Tooltip } from '@radix-ui/themes'
import { useState, type FormEvent } from 'react'
import { usePoll } from '../hooks/usePoll'
import { api, ApiError } from '../lib/api'

/**
 * Per-hostname maintenance switch: listed hostnames answer with the 503
 * maintenance page even while their tunnel clients stay connected. `*`
 * covers every hostname. In-memory only — a server restart clears it.
 */
export function MaintenanceSection() {
  const { data: hosts, refresh } = usePoll(api.maintenance, 10_000)
  const [hostname, setHostname] = useState('')
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)

  const enable = async (e: FormEvent) => {
    e.preventDefault()
    const value = hostname.trim()
    if (!value) return
    setBusy(true)
    setError(null)
    try {
      await api.setMaintenance(value, true)
      setHostname('')
      refresh()
    } catch (err) {
      setError(err instanceof ApiError ? err.message : String(err))
    } finally {
      setBusy(false)
    }
  }

  const disable = async (host: string) => {
    try {
      await api.setMaintenance(host, false)
    } finally {
      refresh()
    }
  }

  return (
    <Flex direction="column" gap="3">
      <Heading size="4">Maintenance Mode</Heading>
      <Card size="3">
        <Flex direction="column" gap="3">
          <form onSubmit={enable}>
            <Flex gap="2" align="center">
              <div style={{ flex: 1, maxWidth: 340 }}>
                <TextField.Root
                  value={hostname}
                  onChange={(e) => setHostname(e.target.value)}
                  placeholder="app.example.com  (* = all hostnames)"
                />
              </div>
              <Button type="submit" loading={busy} variant="soft" color="amber">
                <ExclamationTriangleIcon /> Enable maintenance
              </Button>
            </Flex>
          </form>
          {error && (
            <Callout.Root color="red" size="1">
              <Callout.Text>{error}</Callout.Text>
            </Callout.Root>
          )}
          {!hosts || hosts.length === 0 ? (
            <Text size="2" color="gray">
              No hostnames in maintenance. Visitors of a listed hostname get the 503 page while
              its clients stay connected; cleared on server restart.
            </Text>
          ) : (
            <Flex gap="2" wrap="wrap">
              {hosts.map((h) => (
                <Badge key={h} color="amber" size="2">
                  {h === '*' ? '* (all hostnames)' : h}
                  <Tooltip content="End maintenance">
                    <IconButton
                      size="1"
                      variant="ghost"
                      color="amber"
                      onClick={() => disable(h)}
                      aria-label={`End maintenance for ${h}`}
                    >
                      <Cross2Icon />
                    </IconButton>
                  </Tooltip>
                </Badge>
              ))}
            </Flex>
          )}
        </Flex>
      </Card>
    </Flex>
  )
}
