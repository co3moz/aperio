import { useEffect, useMemo, useRef, useState } from 'react'
import { Card } from '@/components/ui/card'
import { SectionHeader } from './shared'
import { useI18n } from '@/i18n'
import type { ClientDetail, ServerStats } from '@/lib/api'
import { groupClientsByInstance } from '@/lib/clientGroups'

/** Node grid geometry (SVG user units). */
const ROW_H = 64
const TOP_PAD = 28
const NODE_W = 190
const NODE_H = 40
const COL_X = [24, 320, 620]
const WIDTH = COL_X[2] + NODE_W + 24

interface RouteNode {
  key: string
  label: string
  clients: string[] // client ids served by this route
}

/** Routes a client serves: its hostname binds and/or path bind; a client
 *  with no bind serves the catch-all route. Overrides win, like routing. */
function clientRoutes(c: ClientDetail): string[] {
  const routes: string[] = []
  if (c.override_hostname_bind) {
    routes.push(c.override_hostname_bind)
  } else {
    routes.push(...c.hostname_binds)
  }
  const path = c.override_path_bind ?? c.path_bind
  if (path) routes.push(path)
  if (routes.length === 0) routes.push('*')
  return routes
}

function healthTint(c: ClientDetail): string {
  if (!c.enabled || !c.healthy) return 'var(--destructive)'
  if (c.draining || !c.backend_healthy) return 'oklch(0.75 0.15 85)' // amber
  return 'var(--primary)'
}

/** Cubic edge between two node anchor points. */
function edgePath(x1: number, y1: number, x2: number, y2: number): string {
  const mid = (x1 + x2) / 2
  return `M ${x1} ${y1} C ${mid} ${y1}, ${mid} ${y2}, ${x2} ${y2}`
}

/** Per-client request rate from consecutive stats snapshots. */
function useClientRates(clients: ClientDetail[]): Map<string, number> {
  const prev = useRef<Map<string, { count: number; at: number }>>(new Map())
  const [rates, setRates] = useState<Map<string, number>>(new Map())

  useEffect(() => {
    const now = performance.now()
    const next = new Map<string, number>()
    for (const c of clients) {
      const seen = prev.current.get(c.id)
      if (seen && now > seen.at) {
        const perSec = ((c.request_count - seen.count) * 1000) / (now - seen.at)
        next.set(c.id, Math.max(0, perSec))
      }
      prev.current.set(c.id, { count: c.request_count, at: now })
    }
    setRates(next)
  }, [clients])

  return rates
}

function NodeBox({
  x,
  y,
  label,
  sub,
  tint,
  mono,
}: {
  x: number
  y: number
  label: string
  sub?: string
  tint?: string
  mono?: boolean
}) {
  return (
    <g>
      <rect
        x={x}
        y={y}
        width={NODE_W}
        height={NODE_H}
        rx={10}
        className="fill-card stroke-border"
        strokeWidth={1}
      />
      {tint && <circle cx={x + 14} cy={y + NODE_H / 2} r={4} fill={tint} />}
      <text
        x={x + (tint ? 26 : 12)}
        y={y + (sub ? 17 : NODE_H / 2 + 4)}
        className={`fill-foreground text-[12px] ${mono ? 'font-mono' : 'font-medium'}`}
      >
        {label.length > 22 ? `${label.slice(0, 21)}…` : label}
      </text>
      {sub && (
        <text x={x + (tint ? 26 : 12)} y={y + 31} className="fill-muted-foreground text-[10px]">
          {sub.length > 26 ? `${sub.slice(0, 25)}…` : sub}
        </text>
      )}
    </g>
  )
}

export function TopologySection({ stats }: { stats: ServerStats | null }) {
  const { t } = useI18n()
  const clients = useMemo(() => stats?.active_clients ?? [], [stats])
  const rates = useClientRates(clients)

  // One node per (process, service): a client's parallel connections collapse
  // into a single node with a connection count, instead of N look-alikes.
  const groups = useMemo(() => groupClientsByInstance(clients), [clients])

  const routes = useMemo(() => {
    const map = new Map<string, RouteNode>()
    for (const g of groups) {
      for (const r of clientRoutes(g.rep)) {
        const node = map.get(r) ?? { key: r, label: r === '*' ? t('(any request)') : r, clients: [] }
        node.clients.push(g.key)
        map.set(r, node)
      }
    }
    return [...map.values()].sort((a, b) => a.key.localeCompare(b.key))
  }, [groups, t])

  const rows = Math.max(routes.length, groups.length, 1)
  const height = TOP_PAD + rows * ROW_H + 8
  const colY = (count: number, i: number) =>
    TOP_PAD + ((rows - count) * ROW_H) / 2 + i * ROW_H

  const routeY = new Map(routes.map((r, i) => [r.key, colY(routes.length, i)]))
  const clientY = new Map(groups.map((g, i) => [g.key, colY(groups.length, i)]))

  return (
    <section className="flex flex-col gap-3">
      <SectionHeader
        title={t('Topology')}
        description={t('How public routes reach clients and their backends, with live request rates. Green = healthy, amber = draining or failing backend probes, red = unhealthy or disabled.')}
      />
      <Card className="overflow-x-auto p-4">
        {clients.length === 0 ? (
          <p className="py-8 text-center text-sm text-muted-foreground">
            {t('No clients connected')}
          </p>
        ) : (
          <svg
            viewBox={`0 0 ${WIDTH} ${height}`}
            width="100%"
            style={{ minWidth: 720 }}
            role="img"
            aria-label={t('Topology')}
          >
            {/* column captions */}
            {[t('Routes'), t('Tunnel clients'), t('Backends')].map((cap, i) => (
              <text
                key={cap}
                x={COL_X[i] + NODE_W / 2}
                y={14}
                textAnchor="middle"
                className="fill-muted-foreground text-[10px] uppercase tracking-wider"
              >
                {cap}
              </text>
            ))}

            {/* route -> client edges */}
            {routes.flatMap((r) =>
              r.clients.map((cid) => {
                const y1 = (routeY.get(r.key) ?? 0) + NODE_H / 2
                const y2 = (clientY.get(cid) ?? 0) + NODE_H / 2
                return (
                  <path
                    key={`${r.key}->${cid}`}
                    d={edgePath(COL_X[0] + NODE_W, y1, COL_X[1], y2)}
                    className="stroke-border"
                    strokeWidth={1.25}
                    fill="none"
                  />
                )
              }),
            )}

            {/* client -> backend edges, labeled with the live rate (summed
                across the group's connections) */}
            {groups.map((g) => {
              const y = (clientY.get(g.key) ?? 0) + NODE_H / 2
              const hasRate = g.connections.some((c) => rates.get(c.id) !== undefined)
              const rate = g.connections.reduce((s, c) => s + (rates.get(c.id) ?? 0), 0)
              return (
                <g key={`edge-${g.key}`}>
                  <path
                    d={edgePath(COL_X[1] + NODE_W, y, COL_X[2], y)}
                    stroke={healthTint(g.rep)}
                    strokeWidth={1.5}
                    fill="none"
                    opacity={0.7}
                  />
                  {hasRate && (
                    <text
                      x={(COL_X[1] + NODE_W + COL_X[2]) / 2}
                      y={y - 6}
                      textAnchor="middle"
                      className="fill-muted-foreground text-[10px] tabular-nums"
                    >
                      {rate >= 10 ? Math.round(rate) : rate.toFixed(1)} req/s
                    </text>
                  )}
                </g>
              )
            })}

            {/* nodes */}
            {routes.map((r) => (
              <NodeBox
                key={r.key}
                x={COL_X[0]}
                y={routeY.get(r.key) ?? 0}
                label={r.label}
                mono
              />
            ))}
            {groups.map((g) => (
              <NodeBox
                key={g.key}
                x={COL_X[1]}
                y={clientY.get(g.key) ?? 0}
                label={g.rep.service ?? g.rep.id.slice(0, 8)}
                sub={`${g.requestCount} req · v${g.rep.version ?? '?'}${
                  g.connections.length > 1 ? ` · ×${g.connections.length}` : ''
                }`}
                tint={healthTint(g.rep)}
              />
            ))}
            {groups.map((g) => (
              <NodeBox
                key={`backend-${g.key}`}
                x={COL_X[2]}
                y={clientY.get(g.key) ?? 0}
                label={g.rep.backend_healthy ? t('backend healthy') : t('backend failing')}
                sub={g.rep.token_name ?? undefined}
                tint={g.rep.backend_healthy ? 'var(--primary)' : 'var(--destructive)'}
              />
            ))}
          </svg>
        )}
      </Card>
    </section>
  )
}
