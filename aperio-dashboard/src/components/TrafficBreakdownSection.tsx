import { Flex, Grid, Heading, Table, Text } from '@radix-ui/themes'
import type { PeriodStats, ServerStats } from '../lib/api'
import { formatBytes } from '../lib/format'
import { EmptyRow } from './ClientsSection'

const TOP_N = 10

function topEntries(map: Record<string, PeriodStats>): [string, PeriodStats][] {
  return Object.entries(map)
    .sort(([, a], [, b]) => b.requests - a.requests)
    .slice(0, TOP_N)
}

function BreakdownTable({
  title,
  keyHeader,
  map,
  empty,
}: {
  title: string
  keyHeader: string
  map: Record<string, PeriodStats>
  empty: string
}) {
  const entries = topEntries(map)
  return (
    <Flex direction="column" gap="2">
      <Text size="2" weight="medium" color="gray">
        {title}
      </Text>
      <Table.Root variant="surface" size="1">
        <Table.Header>
          <Table.Row>
            <Table.ColumnHeaderCell>{keyHeader}</Table.ColumnHeaderCell>
            <Table.ColumnHeaderCell>Requests</Table.ColumnHeaderCell>
            <Table.ColumnHeaderCell>OK / Failed</Table.ColumnHeaderCell>
            <Table.ColumnHeaderCell>Sent</Table.ColumnHeaderCell>
            <Table.ColumnHeaderCell>Received</Table.ColumnHeaderCell>
          </Table.Row>
        </Table.Header>
        <Table.Body>
          {entries.length === 0 ? (
            <EmptyRow colSpan={5}>{empty}</EmptyRow>
          ) : (
            entries.map(([label, s]) => (
              <Table.Row key={label}>
                <Table.Cell>
                  <Text size="2" style={{ fontFamily: 'var(--code-font-family)', wordBreak: 'break-all' }}>
                    {label === '__other' ? '(other)' : label}
                  </Text>
                </Table.Cell>
                <Table.Cell>{s.requests}</Table.Cell>
                <Table.Cell>
                  <Text size="2">
                    {s.success} / {s.failed}
                  </Text>
                </Table.Cell>
                <Table.Cell>{formatBytes(s.bytes_sent)}</Table.Cell>
                <Table.Cell>{formatBytes(s.bytes_received)}</Table.Cell>
              </Table.Row>
            ))
          )}
        </Table.Body>
      </Table.Root>
    </Flex>
  )
}

/** Lifetime traffic attributed to tokens and hostnames (top 10 each). */
export function TrafficBreakdownSection({ stats }: { stats: ServerStats | null }) {
  return (
    <Flex direction="column" gap="3">
      <Heading size="4">Traffic Breakdown</Heading>
      <Grid columns={{ initial: '1', md: '2' }} gap="4">
        <BreakdownTable
          title="By token"
          keyHeader="Token"
          map={stats?.persistent.by_token ?? {}}
          empty="No attributed traffic yet"
        />
        <BreakdownTable
          title="By hostname"
          keyHeader="Hostname"
          map={stats?.persistent.by_hostname ?? {}}
          empty="No attributed traffic yet"
        />
      </Grid>
    </Flex>
  )
}
