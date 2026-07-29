import { CableIcon, RefreshCwIcon } from 'lucide-react'
import { useCallback, useEffect, useState } from 'react'
import { TintBadge } from './badges'
import { CopyButton, EmptyRow, SectionHeader, SkeletonRows, StatusDot } from './shared'
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
import { api, type DeclaredTunnel } from '@/lib/api'
import { useI18n } from '@/i18n'

/**
 * The tunnel's full address: `<org>@<name>`.
 *
 * A name is unique inside an organization and nowhere else, so the bare one
 * is only an address by accident — and this list shows every organization at
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
  // FNV-1a over the name, folded into 20000..29999 — the same derivation the
  // client uses, so the number shown is the number it will bind.
  let hash = 0xcbf29ce484222325n
  for (const byte of new TextEncoder().encode(tunnel.name)) {
    hash ^= BigInt(byte)
    hash = (hash * 0x100000001b3n) & 0xffffffffffffffffn
  }
  return 20000 + Number(hash % 10000n)
}

/**
 * The tunnels this organization's clients declare: private services that are
 * never routed or exposed publicly, reachable with `--bind-tunnels`.
 *
 * Read-only on purpose. Seeing what exists is a dashboard question; binding
 * one needs a tunnel token carrying `allow_bind`, which is a separate
 * credential precisely so that browsing cannot become reaching.
 */
export function TunnelsSection() {
  const { t } = useI18n()
  const [tunnels, setTunnels] = useState<DeclaredTunnel[] | null>(null)
  const [error, setError] = useState<string | null>(null)

  const load = useCallback(async () => {
    try {
      setTunnels(await api.declaredTunnels())
      setError(null)
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e))
    }
  }, [])

  useEffect(() => {
    void load()
    const timer = setInterval(() => void load(), 10_000)
    return () => clearInterval(timer)
  }, [load])

  return (
    <div className="flex flex-col gap-4">
      <SectionHeader
        title={t('Tunnels')}
        description={t(
          'Private services a client declares but never exposes: a database, an admin port, an SSH daemon. Bind one locally with --bind-tunnels.',
        )}
      >
        <Button variant="outline" size="sm" onClick={() => void load()}>
          <RefreshCwIcon /> {t('Refresh')}
        </Button>
      </SectionHeader>

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
                  'No tunnels declared. A client declares them with a tunnels: list in its aperio.yaml; nothing about them is routed or exposed publicly.',
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
                    <StatusDot active={tunnel.available} />
                    <span>
                      <span className="text-muted-foreground">{tunnel.org ?? 'master'}@</span>
                      {tunnel.name}
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
                  {tunnel.client_id ?? '—'}
                  {tunnel.token_name && (
                    <span className="ml-2 font-sans">{tunnel.token_name}</span>
                  )}
                </TableCell>
                <TableCell className="text-right">
                  <CopyButton value={bindSnippet(tunnel)} label={t('Copy config')} />
                </TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
      </Card>
    </div>
  )
}
