import { cloneElement, isValidElement, useId, useState, type ReactElement, type ReactNode } from 'react'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Dialog, DialogContent, DialogHeader, DialogTitle, DialogDescription, DialogFooter } from '@/components/ui/dialog'
import { useI18n } from '@/i18n'
import { ApiError, exposeApi, type ExposeAction, type ExposeItem, type ExposeLimits, type ExposePolicy, type ExposeProtocol, type ExposeSpec } from '@/lib/api'
import { useOrgLabel, useSession } from '@/lib/session'

export const EXPOSE_ACTIONS: ExposeAction[] = ['read', 'create', 'update', 'enable', 'disable', 'delete', 'disconnect', 'delegate']

export function ExposeActionsEditor({ org, value, onChange }: { org: string; value: ExposeAction[]; onChange: (value: ExposeAction[]) => void }) {
  const { t } = useI18n()
  const { masterAdmin, orgs } = useSession()
  const held = orgs.find((o) => o.id === org)?.expose_actions ?? []
  const labels: Record<ExposeAction, string> = { read: t('Read'), create: t('Create'), update: t('Edit'), enable: t('Enable'), disable: t('Disable'), delete: t('Delete'), disconnect: t('Disconnect'), delegate: t('Delegate') }
  return <fieldset className="grid gap-2 rounded-lg border p-3">
    <legend className="px-1 text-sm font-medium">{t('Public expose permissions')}</legend>
    <p className="text-xs text-muted-foreground">{t('These permissions control server listeners independently of the dashboard role.')}</p>
    <div className="flex flex-wrap gap-x-4 gap-y-2">{EXPOSE_ACTIONS.map((action) => <label key={action} className="flex items-center gap-2 text-sm">
      <input type="checkbox" checked={value.includes(action)} disabled={!masterAdmin && !(held.includes('delegate') && held.includes(action))}
        onChange={(e) => onChange(e.target.checked ? [...value, action] : value.filter((a) => a !== action))} />
      {labels[action]}
    </label>)}</div>
  </fieldset>
}

export function ExposeField({ label, children, error }: { label: string; children: ReactNode; error?: string }) {
  const controlId = useId()
  const errorId = `${controlId}-error`
  const control = isValidElement(children)
    ? cloneElement(children as ReactElement<{ id?: string; 'aria-invalid'?: boolean; 'aria-describedby'?: string }>, { id: controlId, 'aria-invalid': error ? true : undefined, 'aria-describedby': error ? errorId : undefined })
    : children
  return <div className="grid gap-1 text-sm"><label htmlFor={controlId}>{label}</label>{control}{error && <span id={errorId} role="alert" className="text-xs text-destructive">{error}</span>}</div>
}
export const exposeSelectClass = 'w-full rounded-md border bg-background px-3 py-2 text-sm'

export function defaultExposeLimits(protocol: ExposeProtocol): ExposeLimits {
  const bandwidth = { ingress_bytes_per_second: 16777216, egress_bytes_per_second: 16777216 }
  return protocol === 'tcp'
    ? { protocol, max_connections: 256, open_timeout_secs: 10, drain_timeout_secs: 30, ...bandwidth }
    : { protocol, max_sessions: 1024, max_sessions_per_ip: 32, idle_timeout_secs: 60, max_datagram_bytes: 65507, queue_packets: 64, queue_bytes: 262144, new_sessions_per_second: 64, ...bandwidth }
}
export function exposeEndpoint(spec: ExposeSpec): string {
  const { address, port, protocol } = spec.listener
  return `${protocol}://${address.includes(':') ? `[${address}]` : address}:${port}`
}

/** A wildcard is a local binding instruction, never a visitor destination. */
export function visitorEndpoint(spec: ExposeSpec): string | null {
  const address = spec.advertised_host || spec.listener.address
  if (address === '0.0.0.0' || address === '::') return null
  return exposeEndpoint({ ...spec, listener: { ...spec.listener, address } })
}

export function ExposeEditor({ item, initial, policies, onClose, onSaved }: {
  item?: ExposeItem; initial?: Partial<ExposeSpec>; policies: ExposePolicy[]; onClose: () => void; onSaved: () => void
}) {
  const { t } = useI18n()
  const { orgs, selectedOrg, masterAdmin } = useSession()
  const orgLabel = useOrgLabel()
  const targets = orgs.filter((o) => o.id !== '*' && (masterAdmin || o.expose_actions?.includes('create')))
  const [spec, setSpec] = useState<ExposeSpec>(() => structuredClone(item?.resource.spec ?? {
    org_id: initial?.org_id ?? targets.find((o) => o.id === selectedOrg)?.id ?? targets[0]?.id ?? 'master',
    tunnel: initial?.tunnel ?? '', listener: initial?.listener ?? { address: '0.0.0.0', port: 20000, protocol: 'tcp' },
    enabled: false, limits: defaultExposeLimits(initial?.listener?.protocol ?? 'tcp'), allowed_ips: [],
  }))
  const [id] = useState(() => item?.resource.id ?? crypto.randomUUID())
  const [revision, setRevision] = useState(item?.resource.revision ?? 0)
  const [error, setError] = useState<ApiError | Error | null>(null)
  const [current, setCurrent] = useState<Awaited<ReturnType<typeof exposeApi.detail>> | null>(null)
  const [busy, setBusy] = useState(false)
  const [ack, setAck] = useState(false)
  const fieldId = useId()
  const policy = policies.find((p) => p.org_id === spec.org_id) ?? item?.policy
  const bounds = orgs.find((o) => o.id === spec.org_id)?.expose_bounds ?? item?.bounds
  const setLimit = (field: string, value: number) => setSpec((s) => ({ ...s, limits: { ...s.limits, [field]: value } }))
  const limitLabels: Record<string, string> = {
    max_connections: t('Maximum TCP connections'), open_timeout_secs: t('Open timeout (seconds)'), drain_timeout_secs: t('Drain timeout (seconds)'),
    max_sessions: t('Maximum UDP sessions'), max_sessions_per_ip: t('UDP sessions per IP'), idle_timeout_secs: t('Idle timeout (seconds)'),
    max_datagram_bytes: t('Maximum datagram size (bytes)'), queue_packets: t('Queue capacity (packets)'), queue_bytes: t('Queue capacity (bytes)'),
    new_sessions_per_second: t('New UDP sessions per second'), ingress_bytes_per_second: t('Ingress limit (bytes/s)'), egress_bytes_per_second: t('Egress limit (bytes/s)'),
  }
  const submit = async () => {
    if (busy) return
    setBusy(true); setError(null)
    try {
      if (item) await exposeApi.update(id, revision, spec)
      else await exposeApi.create(id, spec)
      onSaved(); onClose()
    } catch (e) { setError(e instanceof Error ? e : new Error(String(e))) }
    finally { setBusy(false) }
  }
  const conflict = error instanceof ApiError && error.code === 'revision_conflict'
  const canToggle = !item || item.actions.includes(item.resource.spec.enabled ? 'disable' : 'enable')
  const fieldError = (field: string) => error instanceof ApiError ? error.fields.filter((e) => e.field === field || e.field.startsWith(`${field}[`)).map((e) => e.message).join(' ') || undefined : undefined
  return <Dialog open onOpenChange={(open) => !open && !busy && onClose()}>
    <DialogContent className="max-h-[90dvh] overflow-y-auto sm:max-w-2xl">
      <DialogHeader><DialogTitle>{item ? t('Edit public expose') : t('Create public expose')}</DialogTitle>
        <DialogDescription>{t('A public listener accepts traffic directly on the server. Visitors do not authenticate with an Aperio token.')}</DialogDescription>
      </DialogHeader>
      <form id={fieldId} className="grid gap-4" onSubmit={(e) => { e.preventDefault(); void submit() }}>
        <div className="grid gap-3 sm:grid-cols-2">
          <ExposeField error={fieldError('org_id')} label={t('Organization')}><select className={exposeSelectClass} value={spec.org_id} disabled={!!item || busy} onChange={(e) => setSpec({ ...spec, org_id: e.target.value })}>
            {item ? <option value={spec.org_id}>{orgLabel(spec.org_id)}</option> : targets.map((o) => <option key={o.id} value={o.id}>{orgLabel(o.id)}</option>)}
          </select></ExposeField>
          <ExposeField error={fieldError('tunnel')} label={t('Tunnel name')}><Input required value={spec.tunnel} onChange={(e) => setSpec({ ...spec, tunnel: e.target.value })} /></ExposeField>
          <ExposeField error={fieldError('limits.protocol')} label={t('Protocol')}><select className={exposeSelectClass} value={spec.listener.protocol} onChange={(e) => {
            const protocol = e.target.value as ExposeProtocol
            setSpec({ ...spec, listener: { ...spec.listener, protocol }, limits: defaultExposeLimits(protocol) })
          }}><option value="tcp">TCP</option><option value="udp">UDP</option></select></ExposeField>
          <ExposeField error={fieldError('listener.address')} label={t('Listen address')}><Input required value={spec.listener.address} onChange={(e) => setSpec({ ...spec, listener: { ...spec.listener, address: e.target.value } })} /></ExposeField>
          <ExposeField error={fieldError('advertised_host')} label={t('Visitor hostname or IP (optional)')}><Input value={spec.advertised_host ?? ''} onChange={(e) => setSpec({ ...spec, advertised_host: e.target.value || null })} /></ExposeField>
          <ExposeField error={fieldError('listener.port')} label={t('Port')}><Input required type="number" min={1} max={65535} step={1} value={spec.listener.port} onChange={(e) => setSpec({ ...spec, listener: { ...spec.listener, port: Number(e.target.value) } })} /></ExposeField>
          <label className="flex items-center gap-2 text-sm"><input type="checkbox" checked={spec.enabled} disabled={!canToggle} onChange={(e) => setSpec({ ...spec, enabled: e.target.checked })} />{t('Enabled')}</label>
        </div>
        <p className="text-xs text-muted-foreground">{t('Offline tunnels can be saved. Encrypted tunnels cannot be exposed publicly.')}</p>
        {policy ? <div className="rounded-lg border p-3 text-sm"><p>{t('Allocated listeners')}</p><ul className="list-inside list-disc">{policy.allocations.map((a, i) => <li key={i} className="font-mono text-xs">{a.protocol} {a.address} : {a.first_port}–{a.last_port}</li>)}</ul>
          <p>{t('Rule quota')}: {policy.max_rules}</p></div>
          : spec.org_id !== 'master' && <p role="alert" className="text-sm text-destructive">{t('This organization needs a server port allocation before an expose can be created.')}</p>}
        {bounds && <div className="rounded-lg border p-3 text-sm"><p>{t('Delegated expose limits')}</p>
          {bounds.allocations.map((a, i) => <p key={i} className="font-mono text-xs">{a.protocol} {a.address} : {a.first_port}–{a.last_port}</p>)}
          <p>{t('Rule quota')}: {bounds.max_rules} · TCP: {bounds.max_tcp_connections} · UDP: {bounds.max_udp_sessions}</p>
          <p>{t('Ingress limit (bytes/s)')}: {bounds.ingress_bytes_per_second} · {t('Egress limit (bytes/s)')}: {bounds.egress_bytes_per_second}</p>
        </div>}
        <ExposeField label={t('Allowed source IPs or CIDRs (one per line; empty allows all)')}><textarea className={`${exposeSelectClass} min-h-20 font-mono`} value={spec.allowed_ips.join('\n')} onChange={(e) => setSpec({ ...spec, allowed_ips: e.target.value.split('\n') })} onBlur={() => setSpec({ ...spec, allowed_ips: spec.allowed_ips.map((s) => s.trim()).filter(Boolean) })} /></ExposeField>
        <fieldset className="grid gap-3 rounded-lg border p-3 sm:grid-cols-2"><legend className="px-1 text-sm font-medium">{t('Limits')}</legend>
          {Object.entries(spec.limits).filter(([key]) => key !== 'protocol').map(([key, value]) => <ExposeField key={key} error={fieldError(`limits.${key}`)} label={limitLabels[key]}><Input required type="number" min={key === 'drain_timeout_secs' ? 0 : 1} max={key === 'max_datagram_bytes' ? 65507 : Number.MAX_SAFE_INTEGER} step={1} value={value} onChange={(e) => setLimit(key, Number(e.target.value))} /></ExposeField>)}
        </fieldset>
        <div className="grid gap-2 rounded-lg bg-muted p-3 text-sm"><code>{exposeEndpoint(spec)}</code>
          <p>{t('This is a bind address. External access also depends on the server firewall, NAT, and container port publishing.')}</p>
          {spec.enabled && <label className="flex items-start gap-2"><input type="checkbox" required checked={ack} onChange={(e) => setAck(e.target.checked)} />{t('I understand that this endpoint accepts public traffic.')}</label>}
        </div>
        {error && <p role="alert" className="whitespace-pre-wrap text-sm text-destructive">{error.message}</p>}
        {conflict && <div className="grid gap-2 rounded-lg border p-3 text-sm"><p>{t('Your draft is preserved. Review the current server version before replacing it.')}</p>
          <Button type="button" variant="outline" onClick={() => void exposeApi.detail(id).then(setCurrent).catch((e: Error) => setError(e))}>{t('Load current version')}</Button>
          {current && <><pre className="max-h-48 overflow-auto text-xs">{JSON.stringify(current.spec, null, 2)}</pre><Button type="button" variant="outline" onClick={() => { setRevision(current.revision); setCurrent(null); setError(null) }}>{t('Keep my draft against this version')}</Button></>}
        </div>}
      </form>
      <DialogFooter><Button variant="outline" disabled={busy} onClick={onClose}>{t('Cancel')}</Button><Button type="submit" form={fieldId} disabled={busy || (spec.enabled && !ack) || (!item && targets.length === 0)}>{busy ? t('Saving…') : t('Save')}</Button></DialogFooter>
    </DialogContent>
  </Dialog>
}
