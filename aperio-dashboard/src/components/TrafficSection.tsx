import { MagnifyingGlassIcon, PauseIcon, PlayIcon } from '@radix-ui/react-icons'
import { Button, Card, Flex, Grid, Heading, Table, Text, TextField, Tooltip } from '@radix-ui/themes'
import { useEffect, useState } from 'react'
import type { RequestLog } from '../lib/api'
import { formatRelativeTime } from '../lib/format'
import { readParams, writeParams } from '../lib/url'
import { EmptyRow, SkeletonRows } from './ClientsSection'
import { MethodBadge, StatusBadge } from './badges'

// Cap the number of rendered rows so a busy tunnel doesn't paint thousands of
// DOM nodes each poll; the newest requests are always the ones kept.
const MAX_ROWS = 200
const METHODS = ['GET', 'POST', 'PUT', 'PATCH', 'DELETE']
type StatusColor = 'green' | 'indigo' | 'amber' | 'red'
const STATUS_FILTERS: { key: string; label: string; color: StatusColor }[] = [
  { key: '2xx', label: '2xx', color: 'green' },
  { key: '3xx', label: '3xx', color: 'indigo' },
  { key: '4xx', label: '4xx', color: 'amber' },
  { key: '5xx', label: '5xx / error', color: 'red' },
]

function statusBucket(log: RequestLog): string {
  if (log.error || log.status == null) return '5xx'
  if (log.status >= 500) return '5xx'
  return `${Math.floor(log.status / 100)}xx`
}

function matchesStatus(log: RequestLog, statusFilter: string | null): boolean {
  if (!statusFilter) return true
  return statusBucket(log) === statusFilter
}

function percentile(sorted: number[], p: number): number {
  if (sorted.length === 0) return 0
  const idx = Math.min(sorted.length - 1, Math.floor((p / 100) * sorted.length))
  return sorted[idx]
}

// A chip toggle: clicking the active value clears it (back to "all").
function FilterChip({
  active,
  color,
  onClick,
  children,
}: {
  active: boolean
  color?: StatusColor
  onClick: () => void
  children: React.ReactNode
}) {
  return (
    <Button size="1" variant={active ? 'solid' : 'soft'} color={active ? color : 'gray'} onClick={onClick}>
      {children}
    </Button>
  )
}

// Latency percentiles and a status-class breakdown over the recent request
// window, so the operator sees tail latency and error share at a glance.
function TrafficStats({ logs }: { logs: RequestLog[] }) {
  const durations = logs.map((l) => l.duration_ms).sort((a, b) => a - b)
  const p50 = percentile(durations, 50)
  const p95 = percentile(durations, 95)
  const p99 = percentile(durations, 99)

  const counts: Record<StatusColor, number> = { green: 0, indigo: 0, amber: 0, red: 0 }
  for (const l of logs) {
    const bucket = statusBucket(l)
    const color = STATUS_FILTERS.find((s) => s.key === bucket)?.color ?? 'red'
    counts[color]++
  }
  const total = logs.length || 1

  return (
    <Grid columns={{ initial: '1', sm: '2' }} gap="3">
      <Card size="2">
        <Text size="1" weight="bold" color="gray" style={{ textTransform: 'uppercase', letterSpacing: '1px' }}>
          Latency (recent {logs.length})
        </Text>
        <Flex gap="5" mt="2">
          {[
            { label: 'p50', v: p50 },
            { label: 'p95', v: p95 },
            { label: 'p99', v: p99 },
          ].map((m) => (
            <Flex key={m.label} direction="column">
              <Text size="1" color="gray">
                {m.label}
              </Text>
              <Text size="5" weight="bold">
                {m.v} ms
              </Text>
            </Flex>
          ))}
        </Flex>
      </Card>
      <Card size="2">
        <Text size="1" weight="bold" color="gray" style={{ textTransform: 'uppercase', letterSpacing: '1px' }}>
          Status mix
        </Text>
        <Flex mt="2" style={{ height: 10, borderRadius: 5, overflow: 'hidden' }}>
          {STATUS_FILTERS.map((s) => {
            const w = (counts[s.color] / total) * 100
            return w > 0 ? (
              <Tooltip key={s.key} content={`${s.label}: ${counts[s.color]}`}>
                <div style={{ width: `${w}%`, background: `var(--${s.color}-9)` }} />
              </Tooltip>
            ) : null
          })}
        </Flex>
        <Flex gap="3" mt="2" wrap="wrap">
          {STATUS_FILTERS.map((s) => (
            <Flex key={s.key} align="center" gap="1">
              <span style={{ width: 8, height: 8, borderRadius: 2, background: `var(--${s.color}-9)` }} />
              <Text size="1" color="gray">
                {s.label} {counts[s.color]}
              </Text>
            </Flex>
          ))}
        </Flex>
      </Card>
    </Grid>
  )
}

export function TrafficSection({
  logs,
  onInspect,
}: {
  logs: RequestLog[] | null
  onInspect: (id: string) => void
}) {
  const [filter, setFilter] = useState(() => readParams().get('q') ?? '')
  const [method, setMethod] = useState<string | null>(() => readParams().get('method'))
  const [statusFilter, setStatusFilter] = useState<string | null>(() => readParams().get('status'))
  const [paused, setPaused] = useState(false)
  const [frozen, setFrozen] = useState<RequestLog[]>([])

  // Reflect the active filters in the URL so the view is shareable/bookmarkable.
  useEffect(() => {
    const params = readParams()
    const apply = (key: string, value: string | null) => {
      if (value) params.set(key, value)
      else params.delete(key)
    }
    apply('q', filter)
    apply('method', method)
    apply('status', statusFilter)
    writeParams(params)
  }, [filter, method, statusFilter])

  const togglePause = () => {
    if (!paused) setFrozen(logs ?? [])
    setPaused((p) => !p)
  }

  const source = paused ? frozen : (logs ?? [])
  const needle = filter.toLowerCase()
  const matched = source.filter(
    (log) =>
      (log.uri.toLowerCase().includes(needle) || log.method.toLowerCase().includes(needle)) &&
      (!method || log.method.toUpperCase() === method) &&
      matchesStatus(log, statusFilter),
  )
  const total = matched.length
  const visible = matched.slice(-MAX_ROWS).reverse()

  return (
    <Flex direction="column" gap="3">
      <Flex justify="between" align="center" gap="3" wrap="wrap">
        <Flex align="center" gap="2">
          <Heading size="4">Live Tunnel Traffic</Heading>
          <Tooltip content={paused ? 'Resume live updates' : 'Freeze the table while you inspect'}>
            <Button size="1" variant="soft" color={paused ? 'amber' : 'gray'} onClick={togglePause}>
              {paused ? <PlayIcon /> : <PauseIcon />} {paused ? 'Paused' : 'Live'}
            </Button>
          </Tooltip>
        </Flex>
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

      {source.length > 0 && <TrafficStats logs={source} />}

      <Flex gap="2" align="center" wrap="wrap">
        <Text size="1" color="gray">
          Method
        </Text>
        {METHODS.map((m) => (
          <FilterChip
            key={m}
            active={method === m}
            color="indigo"
            onClick={() => setMethod((cur) => (cur === m ? null : m))}
          >
            {m}
          </FilterChip>
        ))}
        <Text size="1" color="gray" ml="2">
          Status
        </Text>
        {STATUS_FILTERS.map((s) => (
          <FilterChip
            key={s.key}
            active={statusFilter === s.key}
            color={s.color}
            onClick={() => setStatusFilter((cur) => (cur === s.key ? null : s.key))}
          >
            {s.label}
          </FilterChip>
        ))}
        {(method || statusFilter || filter) && (
          <Button
            size="1"
            variant="ghost"
            color="gray"
            onClick={() => {
              setMethod(null)
              setStatusFilter(null)
              setFilter('')
            }}
          >
            Clear
          </Button>
        )}
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
          {logs === null && !paused ? (
            <SkeletonRows rows={6} cols={6} />
          ) : visible.length === 0 ? (
            <EmptyRow colSpan={6} icon={<MagnifyingGlassIcon />}>
              No requests matching filter
            </EmptyRow>
          ) : (
            visible.map((log) => (
              <Table.Row
                key={log.id}
                className="clickable-row"
                title="Click to inspect & replay"
                onClick={() => onInspect(log.id)}
              >
                <Table.Cell>
                  <Tooltip content={log.timestamp}>
                    <Text size="2" color="gray" style={{ fontFamily: 'var(--code-font-family)' }}>
                      {formatRelativeTime(log.timestamp)}
                    </Text>
                  </Tooltip>
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

      <Flex justify="between" wrap="wrap" gap="2">
        {total > MAX_ROWS ? (
          <Text size="1" color="gray">
            Showing the latest {MAX_ROWS} of {total} matching requests.
          </Text>
        ) : (
          <span />
        )}
        {paused && (
          <Text size="1" color="amber">
            Paused — table frozen at {frozen.length} requests.
          </Text>
        )}
      </Flex>
    </Flex>
  )
}
