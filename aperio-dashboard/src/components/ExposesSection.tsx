import { useState } from 'react'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Card } from '@/components/ui/card'
import { Dialog, DialogContent, DialogHeader, DialogTitle, DialogDescription, DialogFooter } from '@/components/ui/dialog'
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from '@/components/ui/table'
import { useStream } from '@/hooks/useStream'
import { useI18n } from '@/i18n'
import { exposeApi, type ExposeItem, type ExposeSpec } from '@/lib/api'
import { formatBytes } from '@/lib/format'
import { useOrgLabel, useSession } from '@/lib/session'
import { CopyButton, SectionHeader, SkeletonRows } from './shared'
import { Freshness } from './Freshness'
import { ExposeEditor, ExposeField, exposeEndpoint, visitorEndpoint, exposeSelectClass } from './ExposeEditor'
import { ExposePolicies } from './ExposePolicies'

function ExposeSessions({ item, onClose, onOperate }: { item: ExposeItem; onClose: () => void; onOperate: (session: string) => void }) {
  const { t } = useI18n()
  const [history, setHistory] = useState(false)
  const [offset, setOffset] = useState(0)
  const { data: snapshot, error, refresh, updatedAt } = useStream(null, () => exposeApi.sessions(item.resource.id, history, offset), 3000, `${item.resource.id}:${history}:${offset}`)
  const [eventOffset, setEventOffset] = useState(0)
  const audit = useStream(null, () => exposeApi.events(item.resource.id, eventOffset), 10000, `${item.resource.id}:${eventOffset}`)
  const data = error ? null : snapshot
  return <Dialog open onOpenChange={(open) => !open && onClose()}><DialogContent className="max-h-[90dvh] overflow-y-auto sm:max-w-4xl">
    <DialogHeader><DialogTitle>{t('Expose sessions')}: {item.resource.spec.tunnel}</DialogTitle><DialogDescription>{exposeEndpoint(item.resource.spec)}</DialogDescription></DialogHeader>
    <div className="flex items-center justify-between gap-3"><label className="flex gap-2 text-sm"><input type="checkbox" checked={history} onChange={(e) => { setHistory(e.target.checked); setOffset(0) }} />{t('Include recent sessions')}</label><Freshness updatedAt={updatedAt} onRefresh={refresh} /></div>
    {error && <p role="alert" className="text-sm text-destructive">{t('Could not load expose sessions.')}</p>}
    <Table><TableHeader><TableRow><TableHead>{t('Source')}</TableHead><TableHead>{t('Target')}</TableHead><TableHead>{t('Traffic')}</TableHead><TableHead>{t('Status')}</TableHead><TableHead>{t('Actions')}</TableHead></TableRow></TableHeader><TableBody>
      {!data && <SkeletonRows rows={3} cols={5} />}
      {data?.items.map((s) => <TableRow key={s.id}>
        <TableCell className="font-mono text-xs">{s.peer}<div className="font-sans text-muted-foreground">{new Date(s.started_at * 1000).toLocaleString()}</div></TableCell>
        <TableCell className="font-mono text-xs">{s.target}<div className="max-w-40 truncate text-muted-foreground" title={s.client_id}>{s.client_id}</div></TableCell>
        <TableCell className="text-xs">↑ {formatBytes(s.up_bytes)} · ↓ {formatBytes(s.down_bytes)}<div>{s.up_packets} / {s.down_packets}</div></TableCell>
        <TableCell className="text-xs">{s.ended_at ? (s.reason ?? t('Ended')) : t('Active')}<div>{t('Idle: {seconds}s', { seconds: s.idle_seconds })}</div></TableCell>
        <TableCell>{!s.ended_at && item.actions.includes('disconnect') && <Button size="xs" variant="destructive" onClick={() => onOperate(s.id)}>{t('Disconnect')}</Button>}</TableCell>
      </TableRow>)}
      {data?.items.length === 0 && <TableRow><TableCell colSpan={5}>{t('No expose sessions.')}</TableCell></TableRow>}
    </TableBody></Table>
    <div className="flex items-center justify-between text-sm"><span>{data?.total ?? 0} {t('Sessions')}</span><div className="flex gap-2"><Button variant="outline" disabled={offset === 0} onClick={() => setOffset(Math.max(0, offset - 100))}>{t('Previous')}</Button><Button variant="outline" disabled={!data || offset + 100 >= data.total} onClick={() => setOffset(offset + 100)}>{t('Next')}</Button></div></div>
    <details className="rounded-lg border p-3"><summary className="cursor-pointer text-sm font-medium">{t('Audit log')}</summary>
      <div className="mt-3 grid gap-3">
        {!audit.error && audit.data?.items.map((event, index) => <div key={`${event.ts}:${index}`} className="grid gap-1 border-b pb-2 text-xs">
          <p>{event.timestamp} · {event.actor} · {event.event}</p><pre className="max-h-48 overflow-auto whitespace-pre-wrap break-all">{event.details}</pre>
        </div>)}
        {audit.error && <p role="alert" className="text-sm text-destructive">{t('Could not load expose history.')}</p>}
        <div className="flex justify-end gap-2"><Button variant="outline" size="xs" disabled={eventOffset === 0} onClick={() => setEventOffset(Math.max(0, eventOffset - 100))}>{t('Previous')}</Button><Button variant="outline" size="xs" disabled={!audit.data || eventOffset + 100 >= audit.data.total} onClick={() => setEventOffset(eventOffset + 100)}>{t('Next')}</Button></div>
      </div>
    </details>
  </DialogContent></Dialog>
}

export function ExposesSection({ initial, clearInitial, focus }: { initial?: Partial<ExposeSpec>; clearInitial: () => void; focus: { expose?: string; client?: string } | null }) {
  const { t } = useI18n()
  const { orgs, masterAdmin } = useSession()
  const orgLabel = useOrgLabel()
  const [search, setSearch] = useState(focus?.expose ?? '')
  const [protocol, setProtocol] = useState('')
  const [org, setOrg] = useState('')
  const [sort, setSort] = useState('tunnel')
  const [descending, setDescending] = useState(false)
  const [offset, setOffset] = useState(0)
  const [editor, setEditor] = useState<ExposeItem | 'new' | null>(null)
  const [sessions, setSessions] = useState<ExposeItem | null>(null)
  const [operation, setOperation] = useState<{ item: ExposeItem; action: string; session?: string } | null>(null)
  const [drain, setDrain] = useState(30)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const query = { search, protocol, org, offset, sort, descending, client: focus?.client }
  const unfiltered = !focus?.client && !search && !protocol && !org && offset === 0 && sort === 'tunnel' && !descending
  const { data, error: failed, refresh, updatedAt } = useStream(unfiltered ? 'exposes' : null, () => exposeApi.list(query), 3000, JSON.stringify(query))
  const policies = useStream(null, exposeApi.policies, 10000)
  const canCreate = masterAdmin || orgs.some((o) => o.expose_actions?.includes('create'))
  const stateLabels = { disabled: t('Disabled'), binding: t('Binding'), listening: t('Listening'), draining: t('Draining'), suspended: t('Suspended'), failed: t('Failed') }
  const targetLabels = { waiting: t('Waiting for target'), ready: t('Ready'), unavailable: t('Unavailable'), incompatible: t('Incompatible target') }
  const actionLabels: Record<string, string> = { enable: t('Enable'), disable: t('Disable'), retry: t('Retry'), drain: t('Drain'), delete: t('Delete'), disconnect: t('Disconnect') }
  const begin = (item: ExposeItem, action: string, session?: string) => { setOperation({ item, action, session }); setError(null) }
  const execute = async () => {
    if (!operation || busy) return
    setBusy(true); setError(null)
    try {
      const { item: { resource }, action, session } = operation
      if (action === 'delete') await exposeApi.remove(resource.id, resource.revision)
      else await exposeApi.action(resource.id, resource.revision, action, { session_id: session, ...(action === 'drain' ? { drain_seconds: drain } : {}) })
      setOperation(null); refresh()
    } catch (e) { setError(e instanceof Error ? e.message : String(e)); refresh() }
    finally { setBusy(false) }
  }
  return <section className="flex flex-col gap-4" aria-label={t('Public exposes')}>
    <SectionHeader title={t('Public exposes')} description={t('Manage server-side TCP and UDP listeners using your organization permissions.')}>
      <Freshness updatedAt={updatedAt} onRefresh={refresh} />
      {canCreate && <Button size="sm" onClick={() => setEditor('new')}>{t('Create public expose')}</Button>}
    </SectionHeader>
    {data?.volatile && <p role="status" className="text-sm text-amber-600">{t('Changes are volatile and will be lost when the server restarts.')}</p>}
    {[data?.store_error, data?.file_error, data?.history_error, failed ? t('Could not load public exposes.') : null].filter(Boolean).map((message, i) => <p key={i} role="alert" className="text-sm text-destructive">{message}</p>)}
    <div className="grid gap-2 sm:grid-cols-3">
      <ExposeField label={t('Search')}><Input value={search} onChange={(e) => { setSearch(e.target.value); setOffset(0) }} /></ExposeField>
      <ExposeField label={t('Protocol')}><select className={exposeSelectClass} value={protocol} onChange={(e) => { setProtocol(e.target.value); setOffset(0) }}><option value="">{t('All')}</option><option value="tcp">TCP</option><option value="udp">UDP</option></select></ExposeField>
      <ExposeField label={t('Organization')}><select className={exposeSelectClass} value={org} onChange={(e) => { setOrg(e.target.value); setOffset(0) }}><option value="">{t('All')}</option>{orgs.filter((o) => o.id !== '*').map((o) => <option key={o.id} value={o.id}>{orgLabel(o.id)}</option>)}</select></ExposeField>
    </div>
    <div className="flex flex-wrap items-end gap-3">
      <ExposeField label={t('Sort by')}><select className={exposeSelectClass} value={sort} onChange={(e) => { setSort(e.target.value); setOffset(0) }}>
        <option value="tunnel">{t('Name')}</option><option value="port">{t('Port')}</option><option value="state">{t('Status')}</option><option value="sessions">{t('Sessions')}</option><option value="traffic">{t('Traffic')}</option><option value="org">{t('Organization')}</option>
      </select></ExposeField>
      <label className="flex gap-2 text-sm"><input type="checkbox" checked={descending} onChange={(e) => { setDescending(e.target.checked); setOffset(0) }} />{t('Descending')}</label>
    </div>
    <Card className="overflow-hidden py-0"><Table><TableHeader><TableRow>
      <TableHead>{t('Public endpoint')}</TableHead><TableHead>{t('Tunnel')}</TableHead><TableHead>{t('Status')}</TableHead><TableHead>{t('Traffic')}</TableHead><TableHead>{t('Actions')}</TableHead>
    </TableRow></TableHeader><TableBody>
      {!data && <SkeletonRows rows={3} cols={5} />}
      {data?.items.map((item) => { const r = item.resource; const can = (a: typeof item.actions[number]) => item.actions.includes(a)
        return <TableRow key={r.id}>
          <TableCell className="text-xs"><div className="flex items-center gap-1"><code>{exposeEndpoint(r.spec)}</code>{visitorEndpoint(r.spec) && <CopyButton value={visitorEndpoint(r.spec)!} />}</div><span className="text-muted-foreground">{r.source === 'file' ? t('Server configuration file (read-only)') : t('Managed through API')}</span></TableCell>
          <TableCell><div>{r.spec.tunnel}</div><div className="text-xs text-muted-foreground">{orgLabel(r.spec.org_id)}</div></TableCell>
          <TableCell><div>{stateLabels[r.state]}</div><div className="text-xs text-muted-foreground">{targetLabels[r.target_state]}</div>{r.error && <p className="max-w-64 text-xs text-destructive">{r.error}</p>}</TableCell>
          <TableCell className="text-xs"><div>↑ {formatBytes(r.up_bytes)} · ↓ {formatBytes(r.down_bytes)}</div><Button size="xs" variant="ghost" onClick={() => setSessions(item)}>{t('{count} sessions', { count: r.sessions })}</Button>{Object.entries(r.drops).filter(([, n]) => n > 0).map(([reason, n]) => <div key={reason} title={reason}>{t('Dropped')}: {n} ({reason})</div>)}</TableCell>
          <TableCell><div className="flex max-w-72 flex-wrap gap-1">
            {can('update') && <Button size="xs" variant="outline" onClick={() => setEditor(item)}>{t('Edit')}</Button>}
            {r.spec.enabled ? can('disable') && <Button size="xs" variant="outline" onClick={() => begin(item, 'disable')}>{t('Disable')}</Button> : can('enable') && <Button size="xs" variant="outline" onClick={() => begin(item, 'enable')}>{t('Enable')}</Button>}
            {can('enable') && ['failed', 'suspended'].includes(r.state) && <Button size="xs" variant="outline" onClick={() => begin(item, 'retry')}>{t('Retry')}</Button>}
            {r.spec.enabled && can('disable') && can('disconnect') && <Button size="xs" variant="outline" onClick={() => begin(item, 'drain')}>{t('Drain')}</Button>}
            {can('delete') && <Button size="xs" variant="destructive" onClick={() => begin(item, 'delete')}>{t('Delete')}</Button>}
          </div></TableCell>
        </TableRow>
      })}
      {data?.items.length === 0 && <TableRow><TableCell colSpan={5} className="py-8 text-center text-muted-foreground">{t('No public exposes match your access and filters.')}</TableCell></TableRow>}
    </TableBody></Table></Card>
    <div className="flex items-center justify-between text-sm"><span>{data?.total ?? 0} {t('Public exposes')}</span><div className="flex gap-2"><Button variant="outline" disabled={offset === 0} onClick={() => setOffset(Math.max(0, offset - 100))}>{t('Previous')}</Button><Button variant="outline" disabled={!data || offset + 100 >= data.total} onClick={() => setOffset(offset + 100)}>{t('Next')}</Button></div></div>
    <ExposePolicies policies={policies.data ?? []} onSaved={() => { policies.refresh(); refresh() }} />
    {(editor || initial) && <ExposeEditor item={editor && editor !== 'new' ? editor : undefined} initial={initial} policies={policies.data ?? []} onClose={() => { setEditor(null); clearInitial() }} onSaved={refresh} />}
    {sessions && <ExposeSessions item={data?.items.find((i) => i.resource.id === sessions.resource.id) ?? sessions} onClose={() => setSessions(null)} onOperate={(id) => begin(data?.items.find((i) => i.resource.id === sessions.resource.id) ?? sessions, 'disconnect', id)} />}
    {operation && <Dialog open onOpenChange={(open) => !open && !busy && setOperation(null)}><DialogContent>
      <DialogHeader><DialogTitle>{actionLabels[operation.action]} — {operation.item.resource.spec.tunnel}</DialogTitle><DialogDescription>{exposeEndpoint(operation.item.resource.spec)}</DialogDescription></DialogHeader>
      <p className="text-sm">{operation.action === 'enable' || operation.action === 'retry' ? t('This endpoint will accept public traffic without an Aperio visitor token.') : operation.action === 'drain' ? t('New sessions stop immediately. Existing sessions end by the drain deadline.') : operation.action === 'disconnect' ? t('The selected session will be disconnected.') : t('The listener and its active sessions will be stopped.')}</p>
      <p className="text-sm">{t('{count} sessions', { count: operation.item.resource.sessions })}</p>
      {operation.action === 'drain' && <ExposeField label={t('Drain timeout (seconds)')}><Input type="number" min={1} max={3600} value={drain} onChange={(e) => setDrain(Number(e.target.value))} /></ExposeField>}
      {error && <p role="alert" className="text-sm text-destructive">{error}</p>}
      <DialogFooter><Button variant="outline" disabled={busy} onClick={() => setOperation(null)}>{t('Cancel')}</Button><Button disabled={busy || (operation.action === 'drain' && (!Number.isInteger(drain) || drain < 1 || drain > 3600))} onClick={() => void execute()}>{actionLabels[operation.action]}</Button></DialogFooter>
    </DialogContent></Dialog>}
  </section>
}
