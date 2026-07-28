import { CableIcon, RefreshCwIcon } from 'lucide-react'
import { useCallback, useEffect, useState } from 'react'
import { TintBadge } from './badges'
import { CopyButton, EmptyRow, SectionHeader, SkeletonRows, StatusDot } from './shared'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
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

/** The `bind-tunnels:` block that binds one tunnel, ready to paste. */
function bindSnippet(tunnel: DeclaredTunnel): string {
  return ['bind-tunnels:', `  ${tunnel.name}: ${localPortHint(tunnel)}`, ''].join('\n')
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

      <Card>
        <CardContent className="p-0">
          <div className="overflow-x-auto">
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead>{t('Name')}</TableHead>
                  <TableHead>{t('Target')}</TableHead>
                  <TableHead>{t('Protocol')}</TableHead>
                  <TableHead>{t('Client')}</TableHead>
                  <TableHead>{t('Paths')}</TableHead>
                  <TableHead className="text-right">{t('Bind')}</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {tunnels === null && <SkeletonRows rows={3} cols={6} />}
                {tunnels?.length === 0 && (
                  <EmptyRow colSpan={6} icon={<CableIcon />}>
                    {t(
                      'No tunnels declared. A client declares them with a tunnels: list in its aperio.yaml; nothing about them is routed or exposed publicly.',
                    )}
                  </EmptyRow>
                )}
                {tunnels?.map((tunnel) => (
                  <TableRow key={tunnel.name}>
                    <TableCell className="font-mono text-xs">
                      <div className="flex items-center gap-2">
                        <StatusDot active={tunnel.available} />
                        {tunnel.name}
                        {tunnel.encrypt && (
                          <TintBadge tint="blue">{t('encrypted')}</TintBadge>
                        )}
                      </div>
                    </TableCell>
                    <TableCell className="font-mono text-xs text-muted-foreground">
                      {tunnel.target}
                    </TableCell>
                    <TableCell>
                      <TintBadge tint={tunnel.protocol === 'udp' ? 'amber' : 'blue'}>
                        {tunnel.protocol}
                      </TintBadge>
                    </TableCell>
                    <TableCell className="font-mono text-xs text-muted-foreground">
                      {tunnel.client_id ?? '—'}
                      {tunnel.token_name && (
                        <span className="ml-2 font-sans">{tunnel.token_name}</span>
                      )}
                    </TableCell>
                    <TableCell className="text-xs text-muted-foreground">
                      {tunnel.available
                        ? t('{n} available', { n: String(tunnel.paths) })
                        : t('none available')}
                    </TableCell>
                    <TableCell className="text-right">
                      <CopyButton value={bindSnippet(tunnel)} label={t('Copy config')} />
                    </TableCell>
                  </TableRow>
                ))}
              </TableBody>
            </Table>
          </div>
        </CardContent>
      </Card>
    </div>
  )
}
