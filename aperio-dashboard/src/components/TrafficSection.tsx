import { PauseIcon, PlayIcon, SearchIcon } from 'lucide-react'
import { useEffect, useState } from 'react'
import { EmptyRow, SectionHeader, SkeletonRows } from './shared'
import { MethodBadge, StatusBadge } from './badges'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Input } from '@/components/ui/input'
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table'
import { Tooltip, TooltipContent, TooltipTrigger } from '@/components/ui/tooltip'
import type { RequestLog } from '@/lib/api'
import { formatRelativeTime } from '@/lib/format'
import { readParams, writeParams } from '@/lib/url'
import { cn } from '@/lib/utils'

// Cap the number of rendered rows so a busy tunnel doesn't paint thousands of
// DOM nodes each poll; the newest requests are always the ones kept.
const MAX_ROWS = 200
const METHODS = ['GET', 'POST', 'PUT', 'PATCH', 'DELETE']

const STATUS_FILTERS: { key: string; label: string; bar: string; chip: string }[] = [
  {
    key: '2xx',
    label: '2xx',
    bar: 'bg-emerald-500',
    chip: 'data-[active=true]:bg-emerald-500/15 data-[active=true]:text-emerald-700 dark:data-[active=true]:text-emerald-400',
  },
  {
    key: '3xx',
    label: '3xx',
    bar: 'bg-sky-500',
    chip: 'data-[active=true]:bg-sky-500/15 data-[active=true]:text-sky-700 dark:data-[active=true]:text-sky-400',
  },
  {
    key: '4xx',
    label: '4xx',
    bar: 'bg-amber-500',
    chip: 'data-[active=true]:bg-amber-500/15 data-[active=true]:text-amber-700 dark:data-[active=true]:text-amber-400',
  },
  {
    key: '5xx',
    label: '5xx / error',
    bar: 'bg-red-500',
    chip: 'data-[active=true]:bg-red-500/15 data-[active=true]:text-red-700 dark:data-[active=true]:text-red-400',
  },
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
  className,
  onClick,
  children,
}: {
  active: boolean
  className?: string
  onClick: () => void
  children: React.ReactNode
}) {
  return (
    <Button
      size="xs"
      variant="outline"
      data-active={active}
      className={cn('data-[active=true]:border-transparent', className)}
      onClick={onClick}
    >
      {children}
    </Button>
  )
}

// Latency percentiles and a status-class breakdown over the recent request
// window, so the operator sees tail latency and error share at a glance.
function TrafficStats({ logs }: { logs: RequestLog[] }) {
  const durations = logs.map((l) => l.duration_ms).sort((a, b) => a - b)
  const metrics = [
    { label: 'p50', v: percentile(durations, 50) },
    { label: 'p95', v: percentile(durations, 95) },
    { label: 'p99', v: percentile(durations, 99) },
  ]

  const counts: Record<string, number> = { '2xx': 0, '3xx': 0, '4xx': 0, '5xx': 0 }
  for (const l of logs) counts[statusBucket(l)]++
  const total = logs.length || 1

  return (
    <div className="grid grid-cols-1 gap-4 sm:grid-cols-2">
      <Card className="gap-3 py-5">
        <CardHeader className="px-5">
          <CardTitle className="text-xs font-semibold uppercase tracking-wider text-muted-foreground">
            Latency (recent {logs.length})
          </CardTitle>
        </CardHeader>
        <CardContent className="flex gap-8 px-5">
          {metrics.map((m) => (
            <div key={m.label} className="flex flex-col">
              <span className="text-xs text-muted-foreground">{m.label}</span>
              <span className="font-heading text-2xl font-bold tabular-nums">{m.v} ms</span>
            </div>
          ))}
        </CardContent>
      </Card>
      <Card className="gap-3 py-5">
        <CardHeader className="px-5">
          <CardTitle className="text-xs font-semibold uppercase tracking-wider text-muted-foreground">
            Status mix
          </CardTitle>
        </CardHeader>
        <CardContent className="px-5">
          <div className="flex h-2.5 overflow-hidden rounded-full bg-muted">
            {STATUS_FILTERS.map((s) => {
              const w = (counts[s.key] / total) * 100
              return w > 0 ? (
                <Tooltip key={s.key}>
                  <TooltipTrigger
                    render={<div className={s.bar} style={{ width: `${w}%` }} />}
                  />
                  <TooltipContent>
                    {s.label}: {counts[s.key]}
                  </TooltipContent>
                </Tooltip>
              ) : null
            })}
          </div>
          <div className="mt-2 flex flex-wrap gap-3">
            {STATUS_FILTERS.map((s) => (
              <span key={s.key} className="flex items-center gap-1.5 text-xs text-muted-foreground">
                <span className={cn('size-2 rounded-sm', s.bar)} />
                {s.label} {counts[s.key]}
              </span>
            ))}
          </div>
        </CardContent>
      </Card>
    </div>
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
    <section className="flex flex-col gap-3">
      <SectionHeader title="Live Tunnel Traffic">
        <Tooltip>
          <TooltipTrigger
            render={
              <Button
                size="sm"
                variant={paused ? 'secondary' : 'outline'}
                onClick={togglePause}
              />
            }
          >
            {paused ? <PlayIcon /> : <PauseIcon />} {paused ? 'Paused' : 'Live'}
          </TooltipTrigger>
          <TooltipContent>
            {paused ? 'Resume live updates' : 'Freeze the table while you inspect'}
          </TooltipContent>
        </Tooltip>
        <div className="relative">
          <SearchIcon className="pointer-events-none absolute left-2.5 top-1/2 size-4 -translate-y-1/2 text-muted-foreground" />
          <Input
            placeholder="Filter by path/method..."
            value={filter}
            onChange={(e) => setFilter(e.target.value)}
            className="w-64 pl-8"
          />
        </div>
      </SectionHeader>

      {source.length > 0 && <TrafficStats logs={source} />}

      <div className="flex flex-wrap items-center gap-2">
        <span className="text-xs text-muted-foreground">Method</span>
        {METHODS.map((m) => (
          <FilterChip
            key={m}
            active={method === m}
            className="data-[active=true]:bg-primary/15 data-[active=true]:text-primary"
            onClick={() => setMethod((cur) => (cur === m ? null : m))}
          >
            {m}
          </FilterChip>
        ))}
        <span className="ml-2 text-xs text-muted-foreground">Status</span>
        {STATUS_FILTERS.map((s) => (
          <FilterChip
            key={s.key}
            active={statusFilter === s.key}
            className={s.chip}
            onClick={() => setStatusFilter((cur) => (cur === s.key ? null : s.key))}
          >
            {s.label}
          </FilterChip>
        ))}
        {(method || statusFilter || filter) && (
          <Button
            size="xs"
            variant="ghost"
            onClick={() => {
              setMethod(null)
              setStatusFilter(null)
              setFilter('')
            }}
          >
            Clear
          </Button>
        )}
      </div>

      <Card className="overflow-hidden py-0">
        <Table>
          <TableHeader>
            <TableRow>
              <TableHead>Timestamp</TableHead>
              <TableHead>Method</TableHead>
              <TableHead>Path</TableHead>
              <TableHead>Status</TableHead>
              <TableHead>Latency</TableHead>
              <TableHead>Details</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {logs === null && !paused ? (
              <SkeletonRows rows={6} cols={6} />
            ) : visible.length === 0 ? (
              <EmptyRow colSpan={6} icon={<SearchIcon />}>
                No requests matching filter
              </EmptyRow>
            ) : (
              visible.map((log) => (
                <TableRow
                  key={log.id}
                  className="cursor-pointer"
                  title="Click to inspect & replay"
                  onClick={() => onInspect(log.id)}
                >
                  <TableCell>
                    <Tooltip>
                      <TooltipTrigger
                        render={<span className="font-mono text-xs text-muted-foreground" />}
                      >
                        {formatRelativeTime(log.timestamp)}
                      </TooltipTrigger>
                      <TooltipContent>{log.timestamp}</TooltipContent>
                    </Tooltip>
                  </TableCell>
                  <TableCell>
                    <MethodBadge method={log.method} />
                  </TableCell>
                  <TableCell>
                    <span className="inline-block max-w-100 break-all font-mono text-sm">
                      {log.uri}
                    </span>
                  </TableCell>
                  <TableCell>
                    <StatusBadge status={log.status} error={log.error} />
                  </TableCell>
                  <TableCell className="font-mono text-sm tabular-nums">
                    {log.duration_ms} ms
                  </TableCell>
                  <TableCell>
                    {log.error ? (
                      <span className="text-xs text-destructive">{log.error}</span>
                    ) : (
                      <span className="text-sm text-muted-foreground">Success</span>
                    )}
                  </TableCell>
                </TableRow>
              ))
            )}
          </TableBody>
        </Table>
      </Card>

      <div className="flex flex-wrap justify-between gap-2">
        {total > MAX_ROWS ? (
          <span className="text-xs text-muted-foreground">
            Showing the latest {MAX_ROWS} of {total} matching requests.
          </span>
        ) : (
          <span />
        )}
        {paused && (
          <span className="text-xs text-amber-600 dark:text-amber-400">
            Paused — table frozen at {frozen.length} requests.
          </span>
        )}
      </div>
    </section>
  )
}
