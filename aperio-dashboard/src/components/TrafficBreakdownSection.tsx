import { EmptyRow, SectionHeader } from './shared'
import { Card } from '@/components/ui/card'
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table'
import type { PeriodStats, ServerStats } from '@/lib/api'
import { formatBytes } from '@/lib/format'

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
    <div className="flex flex-col gap-2">
      <span className="text-sm font-medium text-muted-foreground">{title}</span>
      <Card className="overflow-hidden py-0">
        <Table>
          <TableHeader>
            <TableRow>
              <TableHead>{keyHeader}</TableHead>
              <TableHead>Requests</TableHead>
              <TableHead>OK / Failed</TableHead>
              <TableHead>Sent</TableHead>
              <TableHead>Received</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {entries.length === 0 ? (
              <EmptyRow colSpan={5}>{empty}</EmptyRow>
            ) : (
              entries.map(([label, s]) => (
                <TableRow key={label}>
                  <TableCell className="break-all font-mono text-sm">
                    {label === '__other' ? '(other)' : label}
                  </TableCell>
                  <TableCell className="tabular-nums">{s.requests}</TableCell>
                  <TableCell className="tabular-nums">
                    {s.success} / {s.failed}
                  </TableCell>
                  <TableCell>{formatBytes(s.bytes_sent)}</TableCell>
                  <TableCell>{formatBytes(s.bytes_received)}</TableCell>
                </TableRow>
              ))
            )}
          </TableBody>
        </Table>
      </Card>
    </div>
  )
}

/** Lifetime traffic attributed to tokens and hostnames (top 10 each). */
export function TrafficBreakdownSection({ stats }: { stats: ServerStats | null }) {
  return (
    <section className="flex flex-col gap-3">
      <SectionHeader title="Traffic Breakdown" />
      <div className="grid grid-cols-1 gap-4 lg:grid-cols-2">
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
      </div>
    </section>
  )
}
