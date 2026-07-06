import { Badge, Code, Flex, Heading, Table, Text } from '@radix-ui/themes'
import { usePoll } from '../hooks/usePoll'
import { api } from '../lib/api'
import { EmptyRow } from './ClientsSection'

export function AuditSection() {
  const { data: events } = usePoll(api.audit, 10_000)

  return (
    <Flex direction="column" gap="3">
      <Heading size="4">Audit Log</Heading>
      <Table.Root variant="surface">
        <Table.Header>
          <Table.Row>
            <Table.ColumnHeaderCell>Time</Table.ColumnHeaderCell>
            <Table.ColumnHeaderCell>Event</Table.ColumnHeaderCell>
            <Table.ColumnHeaderCell>Actor IP</Table.ColumnHeaderCell>
            <Table.ColumnHeaderCell>Details</Table.ColumnHeaderCell>
          </Table.Row>
        </Table.Header>
        <Table.Body>
          {!events || events.length === 0 ? (
            <EmptyRow colSpan={4}>No audit events</EmptyRow>
          ) : (
            [...events].reverse().map((ev, i) => (
              <Table.Row key={`${ev.ts}-${i}`}>
                <Table.Cell>
                  <Text size="2" color="gray" style={{ fontFamily: 'var(--code-font-family)' }}>
                    {ev.timestamp}
                  </Text>
                </Table.Cell>
                <Table.Cell>
                  <Badge color="gray">{ev.event}</Badge>
                </Table.Cell>
                <Table.Cell>
                  <Code size="2">{ev.actor_ip}</Code>
                </Table.Cell>
                <Table.Cell>
                  <Text size="2" style={{ fontFamily: 'var(--code-font-family)', wordBreak: 'break-all' }}>
                    {ev.details}
                  </Text>
                </Table.Cell>
              </Table.Row>
            ))
          )}
        </Table.Body>
      </Table.Root>
    </Flex>
  )
}
