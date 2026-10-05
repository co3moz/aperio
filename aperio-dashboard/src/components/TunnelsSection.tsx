import { useState } from 'react'
import { Button } from '@/components/ui/button'
import { useSession } from '@/lib/session'
import { ExposesSection } from './ExposesSection'
import { CableIcon } from 'lucide-react'
import { TintBadge } from './badges'
import { CopyButton, EmptyRow, SectionHeader, SkeletonRows, StatusDot } from './shared'
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
import { api, type DeclaredTunnel, type ExposeSpec } from '@/lib/api'
import { NO_VALUE } from '@/lib/format'
import { Freshness } from './Freshness'
import { useStream } from '@/hooks/useStream'
import { useI18n } from '@/i18n'

/**
 * The tunnel's full address: `<org>@<name>`.
 *
 * A name is unique inside an organization and nowhere else, so the bare one
 * is only an address by accident, and this list shows every organization at
 * once when read from the master one. The same spelling is what `expose:` and
 * a `bind-tunnels:` key accept.
 */
function qualified(tunnel: DeclaredTunnel): string {
  return `${tunnel.org ?? 'master'}@${tunnel.name}`
}

/** The `bind-tunnels:` block that binds one tunnel, ready to paste. */
function bindSnippet(tunnel: DeclaredTunnel): string {
  return ['bind-tunnels:', `  ${qualified(tunnel)}: ${localPortHint(tunnel)}`, ''].join('\n')
}

/**
 * The local port the binder would pick, mirroring the client's own rule: the
 * declared port when it is unprivileged, otherwise a name-derived one. Shown
 * so the snippet is a complete answer rather than a starting point.
 */
function localPortHint(tunnel: DeclaredTunnel): number {
  const declared = Number(tunnel.target.split(':').pop())
  if (Number.isFinite(declared) && declared >= 1024) return declared
  // FNV-1a over the name, folded into 20000..29999, the same derivation the
  // client uses, so the number shown is the number it will bind.
  let hash = 0xcbf29ce484222325n
  for (const byte of new TextEncoder().encode(tunnel.name)) {
    hash ^= BigInt(byte)
    hash = (hash * 0x100000001b3n) & 0xffffffffffffffffn
  }
  return 20000 + Number(hash % 10000n)
}

export function TunnelsSection({ focus, clearFocus }: { focus: { expose?: string; client?: string } | null; clearFocus: () => void }) {
  const { t } = useI18n()
  const { orgs, masterAdmin } = useSession()
  const [exposeDraft, setExposeDraft] = useState<Partial<ExposeSpec> | undefined>()
  const exposeOrg = (tunnel: DeclaredTunnel) => tunnel.org ? orgs.find((o) => o.name === tunnel.org)?.id : 'master'
  const canExpose = (tunnel: DeclaredTunnel) => {
    const org = exposeOrg(tunnel)
    return !!org && !tunnel.encrypt && (masterAdmin || !!orgs.find((o) => o.id === org)?.expose_actions?.includes('create'))
  }
  const {
    data: declarations,
    refresh: load,
    error: failed,
    updatedAt,
  } = useStream<DeclaredTunnel[]>('tunnels', api.declaredTunnels, 10_000)
  const tunnels = declarations?.filter((tunnel) => !focus?.client || tunnel.client_id === focus.client) ?? null
  const error = failed ? t('Could not load the tunnels; retrying.') : null

  return (
    <div className="flex flex-col gap-4">
      <SectionHeader
        title={t('Tunnels')}
        description={t(
          'Declared TCP and UDP services. Bind them locally or publish a server endpoint with an explicit expose permission.',
        )}
      >
        <Freshness updatedAt={updatedAt} onRefresh={() => void load()} />
      </SectionHeader>

      {focus && <Button variant="outline" size="sm" onClick={clearFocus}>{t('All')}</Button>}
      {error && <p className="text-sm text-destructive">{error}</p>}

      {/* `py-0` because the table is the card: the card's own vertical padding
          would otherwise leave a band above the header row and below the last
          one, which reads as a broken table rather than as spacing. Same
          shape as the other table sections. */}
      <Card className="overflow-hidden py-0">
        <Table>
          <TableHeader>
            <TableRow>
              <TableHead>{t('Name')}</TableHead>
              <TableHead>{t('Target')}</TableHead>
              <TableHead>{t('Protocol')}</TableHead>
              <TableHead>{t('Client')}</TableHead>
              <TableHead className="text-right">{t('Bind')}</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {tunnels === null && <SkeletonRows rows={3} cols={5} />}
            {tunnels?.length === 0 && (
              <EmptyRow colSpan={5} icon={<CableIcon />}>
                {t(
                  'No tunnels declared. Add a tunnels: list in the client configuration to make a service available for binding or public exposure.',
                )}
              </EmptyRow>
            )}
            {tunnels?.map((tunnel) => (
              <TableRow key={qualified(tunnel)}>
                <TableCell className="font-mono text-xs">
                  <div className="flex items-center gap-2">
                    {/* The dot is the availability signal: a tunnel nothing
                        can serve right now is still worth listing, but it
                        should not look bindable. */}
                    <StatusDot active={tunnel.available} label={tunnel.available ? t('Available') : t('Unavailable')} />
                    <span className="flex flex-col">
                      {/* The address is what you paste; the label, when the
                          declaring client gave one, is what you recognize. */}
                      {tunnel.custom_name && (
                        <span className="font-sans text-sm">{tunnel.custom_name}</span>
                      )}
                      <span>
                        <span className="text-muted-foreground">{tunnel.org ?? 'master'}@</span>
                        {tunnel.name}
                      </span>
                    </span>
                    {tunnel.encrypt && <TintBadge tint="blue">{t('encrypted')}</TintBadge>}
                  </div>
                </TableCell>
                <TableCell className="font-mono text-xs text-muted-foreground">
                  {tunnel.target}
                </TableCell>
                <TableCell>
                  {/* A `tcp/udp` tunnel is one tunnel on both transports, so
                      it gets one badge per transport rather than a single
                      label nobody can scan. */}
                  <div className="flex flex-wrap items-center gap-1">
                    {tunnel.protocol.split('/').map((transport) => (
                      <TintBadge key={transport} tint={transport === 'udp' ? 'amber' : 'blue'}>
                        {transport}
                      </TintBadge>
                    ))}
                  </div>
                </TableCell>
                <TableCell className="font-mono text-xs text-muted-foreground">
                  {/* Same address shape as the tunnel's own name above: a
                      service name is unique inside an organization and
                      nowhere else, so `org@name` is the whole address. The
                      connection id is a uuid unless an operator wrote
                      `client_id:`, so it goes in the tooltip. */}
                  {tunnel.client_name ? (
                    <Tooltip>
                      <TooltipTrigger render={<span className="cursor-default" />}>
                        <span className="text-muted-foreground">{tunnel.org ?? 'master'}@</span>
                        {tunnel.client_name}
                      </TooltipTrigger>
                      <TooltipContent>
                        {t('Connection id: {id}', { id: tunnel.client_id ?? NO_VALUE })}
                      </TooltipContent>
                    </Tooltip>
                  ) : (
                    (tunnel.client_id ?? NO_VALUE)
                  )}
                  {tunnel.token_name && (
                    <span className="ml-2 font-sans">{tunnel.token_name}</span>
                  )}
                </TableCell>
                <TableCell className="text-right">
                  <div className="flex justify-end gap-2">
                    <CopyButton value={bindSnippet(tunnel)} label={t('Copy config')} />
                    {canExpose(tunnel) && <Button size="xs" variant="outline" onClick={() => setExposeDraft({ org_id: exposeOrg(tunnel), tunnel: tunnel.name, listener: { address: '0.0.0.0', port: localPortHint(tunnel), protocol: tunnel.protocol === 'udp' ? 'udp' : 'tcp' } })}>{t('Expose publicly')}</Button>}
                  </div>
                </TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
      </Card>
      <ExposesSection key={JSON.stringify(focus)} focus={focus} initial={exposeDraft} clearInitial={() => setExposeDraft(undefined)} />
    </div>
  )
}
