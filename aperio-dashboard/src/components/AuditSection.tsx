import { FileTextIcon, RotateCwIcon } from 'lucide-react'
import { EmptyRow, SectionHeader, SkeletonRows } from './shared'
import { TintBadge } from './badges'
import { Button } from '@/components/ui/button'
import { Card } from '@/components/ui/card'
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table'
import { Tooltip, TooltipContent, TooltipTrigger } from '@/components/ui/tooltip'
import { usePoll } from '@/hooks/usePoll'
import { api } from '@/lib/api'
import { formatRelativeTime } from '@/lib/format'

export function AuditSection() {
  const { data: events, refresh } = usePoll(api.audit, 10_000)

  return (
    <section className="flex flex-col gap-3">
      <SectionHeader title="Audit Log">
        <Tooltip>
          <TooltipTrigger
            render={<Button size="icon-sm" variant="outline" onClick={refresh} aria-label="Refresh" />}
          >
            <RotateCwIcon />
          </TooltipTrigger>
          <TooltipContent>Refresh</TooltipContent>
        </Tooltip>
      </SectionHeader>
      <Card className="overflow-hidden py-0">
        <Table>
          <TableHeader>
            <TableRow>
              <TableHead>Time</TableHead>
              <TableHead>Event</TableHead>
              <TableHead>Actor IP</TableHead>
              <TableHead>Details</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {events === null ? (
              <SkeletonRows rows={5} cols={4} />
            ) : events.length === 0 ? (
              <EmptyRow colSpan={4} icon={<FileTextIcon />}>
                No audit events
              </EmptyRow>
            ) : (
              [...events].reverse().map((ev, i) => (
                <TableRow key={`${ev.ts}-${i}`}>
                  <TableCell>
                    <Tooltip>
                      <TooltipTrigger
                        render={<span className="font-mono text-xs text-muted-foreground" />}
                      >
                        {formatRelativeTime(ev.ts)}
                      </TooltipTrigger>
                      <TooltipContent>{ev.timestamp}</TooltipContent>
                    </Tooltip>
                  </TableCell>
                  <TableCell>
                    <TintBadge tint="gray">{ev.event}</TintBadge>
                  </TableCell>
                  <TableCell>
                    <code className="font-mono text-xs">{ev.actor_ip}</code>
                  </TableCell>
                  <TableCell>
                    <span className="break-all font-mono text-xs">{ev.details}</span>
                  </TableCell>
                </TableRow>
              ))
            )}
          </TableBody>
        </Table>
      </Card>
    </section>
  )
}
