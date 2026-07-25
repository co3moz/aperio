import { GaugeIcon, PowerOffIcon, Trash2Icon, ZapIcon } from 'lucide-react'
import { toast } from 'sonner'
import { EmptyRow, SectionHeader, SkeletonRows } from './shared'
import { TintBadge } from './badges'
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
  AlertDialogTrigger,
} from '@/components/ui/alert-dialog'
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
import { useI18n } from '@/i18n'
import { api, ApiError, type ScalingRecord } from '@/lib/api'
import { useHasRole } from '@/lib/session'

/** Utilization bar: green under the target, amber at or above it. */
function UtilizationBar({ record }: { record: ScalingRecord }) {
  const { t } = useI18n()
  if (record.capacity === 0) {
    // No client announced a concurrency limit, so there is nothing to divide
    // by. Cold starts still work; scale-out has no signal to act on.
    return (
      <Tooltip>
        <TooltipTrigger render={<span className="text-sm text-muted-foreground" />}>
          {t('not measurable')}
        </TooltipTrigger>
        <TooltipContent>
          {t('Set max_concurrent on the client to make utilization measurable.')}
        </TooltipContent>
      </Tooltip>
    )
  }
  const percent = Math.min(100, Math.round(record.utilization * 100))
  const saturated = record.utilization >= record.target_utilization
  return (
    <div className="flex items-center gap-2">
      <div className="h-1.5 w-20 overflow-hidden rounded-full bg-muted">
        <div
          className={saturated ? 'h-full bg-amber-500' : 'h-full bg-emerald-500'}
          style={{ width: `${percent}%` }}
        />
      </div>
      <Tooltip>
        <TooltipTrigger render={<span className="tabular-nums text-sm text-muted-foreground" />}>
          {percent}%
        </TooltipTrigger>
        <TooltipContent>
          {t('{inflight} of {capacity} concurrent slots in use; scales out above {target}%.', {
            inflight: record.inflight,
            capacity: record.capacity,
            target: Math.round(record.target_utilization * 100),
          })}
        </TooltipContent>
      </Tooltip>
    </div>
  )
}

function DisarmButton({ record, onDone }: { record: ScalingRecord; onDone: () => void }) {
  const { t } = useI18n()
  const disarm = async () => {
    try {
      await api.disarmScaling(record.id)
      toast.info(t('Disarmed autoscaling for {host}', { host: record.hostname }))
      onDone()
    } catch (e) {
      toast.error(e instanceof ApiError ? e.message : String(e))
    }
  }
  return (
    <AlertDialog>
      <AlertDialogTrigger render={<Button size="xs" variant="destructive" />}>
        <Trash2Icon /> {t('Disarm')}
      </AlertDialogTrigger>
      <AlertDialogContent>
        <AlertDialogTitle>{t('Disarm autoscaling for "{host}"?', { host: record.hostname })}</AlertDialogTitle>
        <AlertDialogHeader>
          <AlertDialogDescription>
            {t('The server stops calling this endpoint, so the service will no longer cold start or scale out. A client that is still running and still declares the block re-arms it on its next heartbeat.')}
          </AlertDialogDescription>
        </AlertDialogHeader>
        <AlertDialogFooter>
          <AlertDialogCancel>{t('Cancel')}</AlertDialogCancel>
          <AlertDialogAction
            className="bg-destructive/10 text-destructive hover:bg-destructive/20"
            onClick={() => void disarm()}
          >
            {t('Disarm')}
          </AlertDialogAction>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  )
}

/**
 * Autoscaling records armed by clients: what the server will call when a bind
 * needs capacity, and how loaded that bind's pool is right now. Records are
 * created by clients declaring a `scaling:` block, never here, so this view is
 * read-only apart from disarming one.
 */
export function ScalingSection() {
  const { t } = useI18n()
  const canMutate = useHasRole('operator')
  const { data: records, refresh } = usePoll(api.scaling, 10_000)

  return (
    // min-w-0: this table has more columns than most, and without it the flex
    // item refuses to shrink below its content width and pushes the whole page
    // sideways instead of letting the table scroll inside its own card.
    <section className="flex min-w-0 flex-col gap-3">
      <SectionHeader
        title={t('Autoscaling')}
        description={t('Services that ask for capacity when they need it. A request for a hostname nothing is serving triggers a cold start instead of a 504, and a saturated pool asks for one more instance. Declared by the client in its scaling: block; scaling back in is the client’s own idle_timeout.')}
      />
      <Card className="overflow-hidden py-0">
        <Table>
          <TableHeader>
            <TableRow>
              <TableHead>{t('Hostname')}</TableHead>
              <TableHead>{t('Instances')}</TableHead>
              <TableHead>{t('Utilization')}</TableHead>
              <TableHead>{t('Endpoint')}</TableHead>
              <TableHead>{t('State')}</TableHead>
              <TableHead className="text-right">{t('Actions')}</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {records === null ? (
              <SkeletonRows rows={2} cols={6} />
            ) : records.length === 0 ? (
              <EmptyRow colSpan={6} icon={<GaugeIcon />}>
                {t('No autoscaling records. A client arms one by declaring a scaling: block, and the server must have APERIO_SCALING enabled.')}
              </EmptyRow>
            ) : (
              records.map((r) => (
                <TableRow key={r.id}>
                  <TableCell className="font-medium">
                    <div className="flex items-center gap-1.5">
                      {r.hostname}
                      {r.path && (
                        <span className="font-mono text-xs text-muted-foreground">{r.path}</span>
                      )}
                      {r.min === 0 && (
                        <Tooltip>
                          <TooltipTrigger render={<span />}>
                            <TintBadge tint="blue">
                              <ZapIcon className="size-3" /> {t('scale to zero')}
                            </TintBadge>
                          </TooltipTrigger>
                          <TooltipContent>
                            {t('A request for this hostname is held for up to {secs}s while the service starts, instead of failing.', { secs: r.cold_start_secs })}
                          </TooltipContent>
                        </Tooltip>
                      )}
                    </div>
                  </TableCell>
                  <TableCell className="tabular-nums">
                    <Tooltip>
                      <TooltipTrigger render={<span />}>
                        {r.instances}
                        <span className="text-muted-foreground">
                          {' / '}
                          {r.max === 0 ? '\u221e' : r.max}
                        </span>
                      </TooltipTrigger>
                      <TooltipContent>
                        {r.max === 0
                          ? t('Cold starts only: no scale-out ceiling is configured.')
                          : t('The server never asks for more than {max} instances.', { max: r.max })}
                      </TooltipContent>
                    </Tooltip>
                  </TableCell>
                  <TableCell>
                    <UtilizationBar record={r} />
                  </TableCell>
                  <TableCell>
                    <div className="flex items-center gap-1.5">
                      <span className="max-w-[15rem] truncate font-mono text-xs text-muted-foreground">
                        {r.url}
                      </span>
                      {r.authenticated && (
                        <Tooltip>
                          <TooltipTrigger render={<span />}>
                            <TintBadge tint="gray">{t('signed')}</TintBadge>
                          </TooltipTrigger>
                          <TooltipContent>
                            {t('The call carries the client’s secret as a bearer token. The secret itself is never shown.')}
                          </TooltipContent>
                        </Tooltip>
                      )}
                    </div>
                  </TableCell>
                  <TableCell>
                    {r.disarmed ? (
                      <Tooltip>
                        <TooltipTrigger render={<span />}>
                          <TintBadge tint="red">
                            <PowerOffIcon className="size-3" /> {t('disarmed')}
                          </TintBadge>
                        </TooltipTrigger>
                        <TooltipContent>
                          {t('Too many consecutive failures: the endpoint is no longer called. Fix it and restart the client to re-arm.')}
                        </TooltipContent>
                      </Tooltip>
                    ) : (
                      <TintBadge tint="green">{t('armed')}</TintBadge>
                    )}
                  </TableCell>
                  <TableCell>
                    <div className="flex justify-end">
                      {canMutate ? (
                        <DisarmButton record={r} onDone={refresh} />
                      ) : (
                        <span className="text-muted-foreground">-</span>
                      )}
                    </div>
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
