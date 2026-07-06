import { MagnifyingGlassIcon } from '@radix-ui/react-icons'
import { Flex, Heading, Table, Text, TextField } from '@radix-ui/themes'
import { useState } from 'react'
import type { RequestLog } from '../lib/api'
import { EmptyRow } from './ClientsSection'
import { MethodBadge, StatusBadge } from './badges'

export function TrafficSection({
  logs,
  onInspect,
}: {
  logs: RequestLog[] | null
  onInspect: (id: string) => void
}) {
  const [filter, setFilter] = useState('')
  const needle = filter.toLowerCase()
  const filtered = (logs ?? [])
    .filter((log) => log.uri.toLowerCase().includes(needle) || log.method.toLowerCase().includes(needle))
    .reverse()

  return (
    <Flex direction="column" gap="3">
      <Flex justify="between" align="center" gap="3">
        <Heading size="4">Live Tunnel Traffic</Heading>
        <TextField.Root
          placeholder="Filter by path/method..."
          value={filter}
          onChange={(e) => setFilter(e.target.value)}
          style={{ width: 260 }}
        >
          <TextField.Slot>
            <MagnifyingGlassIcon />
          </TextField.Slot>
        </TextField.Root>
      </Flex>
      <Table.Root variant="surface">
        <Table.Header>
          <Table.Row>
            <Table.ColumnHeaderCell>Timestamp</Table.ColumnHeaderCell>
            <Table.ColumnHeaderCell>Method</Table.ColumnHeaderCell>
            <Table.ColumnHeaderCell>Path</Table.ColumnHeaderCell>
            <Table.ColumnHeaderCell>Status</Table.ColumnHeaderCell>
            <Table.ColumnHeaderCell>Latency</Table.ColumnHeaderCell>
            <Table.ColumnHeaderCell>Details</Table.ColumnHeaderCell>
          </Table.Row>
        </Table.Header>
        <Table.Body>
          {logs === null ? (
            <EmptyRow colSpan={6}>Connecting to server data...</EmptyRow>
          ) : filtered.length === 0 ? (
            <EmptyRow colSpan={6}>No requests matching filter</EmptyRow>
          ) : (
            filtered.map((log) => (
              <Table.Row
                key={log.id}
                className="clickable-row"
                title="Click to inspect & replay"
                onClick={() => onInspect(log.id)}
              >
                <Table.Cell>
                  <Text size="2" color="gray" style={{ fontFamily: 'var(--code-font-family)' }}>
                    {log.timestamp}
                  </Text>
                </Table.Cell>
                <Table.Cell>
                  <MethodBadge method={log.method} />
                </Table.Cell>
                <Table.Cell>
                  <Text
                    size="2"
                    style={{
                      fontFamily: 'var(--code-font-family)',
                      wordBreak: 'break-all',
                      maxWidth: 400,
                      display: 'inline-block',
                    }}
                  >
                    {log.uri}
                  </Text>
                </Table.Cell>
                <Table.Cell>
                  <StatusBadge status={log.status} error={log.error} />
                </Table.Cell>
                <Table.Cell>
                  <Text size="2" style={{ fontFamily: 'var(--code-font-family)' }}>
                    {log.duration_ms} ms
                  </Text>
                </Table.Cell>
                <Table.Cell>
                  {log.error ? (
                    <Text size="1" color="red">
                      {log.error}
                    </Text>
                  ) : (
                    <Text size="2">Success</Text>
                  )}
                </Table.Cell>
              </Table.Row>
            ))
          )}
        </Table.Body>
      </Table.Root>
    </Flex>
  )
}
