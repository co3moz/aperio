import { useId, useState } from 'react'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Dialog, DialogContent, DialogHeader, DialogTitle, DialogDescription, DialogFooter } from '@/components/ui/dialog'
import { useStream } from '@/hooks/useStream'
import { useI18n } from '@/i18n'
import { exposeApi, type ExposePolicy, type ExposeProtocol } from '@/lib/api'
import { useOrgLabel, useSession } from '@/lib/session'
import { ExposeField, exposeSelectClass } from './ExposeEditor'

function PolicyEditor({ initial, onClose, onSaved, commit }: { initial: ExposePolicy; onClose: () => void; onSaved: () => void; commit?: (policy: ExposePolicy) => void }) {
  const { t } = useI18n()
  const orgLabel = useOrgLabel()
  const formId = useId()
  const [policy, setPolicy] = useState(() => structuredClone(initial))
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)
  const limits = {
    max_rules: t('Rule quota'), max_tcp_connections: t('Maximum TCP connections'), max_udp_sessions: t('Maximum UDP sessions'),
    ingress_bytes_per_second: t('Ingress limit (bytes/s)'), egress_bytes_per_second: t('Egress limit (bytes/s)'),
  }
  const submit = async () => {
    if (busy) return
    setBusy(true); setError(null)
    try { if (commit) commit(policy); else await exposeApi.setPolicy(policy); onSaved(); onClose() }
    catch (e) { setError(e instanceof Error ? e.message : String(e)) }
    finally { setBusy(false) }
  }
  return <Dialog open onOpenChange={(open) => !open && !busy && onClose()}><DialogContent className="max-h-[90dvh] overflow-y-auto sm:max-w-3xl">
    <DialogHeader><DialogTitle>{t('Port allocation')} — {orgLabel(policy.org_id)}</DialogTitle><DialogDescription>{commit ? t('These limits restrict this identity. Organization-owned listeners keep their lifetime when a user loses access.') : t('Narrowing allocations or quotas suspends incompatible listeners. TCP and UDP have separate port spaces.')}</DialogDescription></DialogHeader>
    <form id={formId} className="grid gap-4" onSubmit={(e) => { e.preventDefault(); void submit() }}>
      <fieldset className="grid gap-3"><legend className="mb-2 text-sm font-medium">{t('Allocated listeners')}</legend>
        {policy.allocations.map((a, i) => <div key={i} className="grid grid-cols-2 items-end gap-2 rounded-lg border p-3 sm:grid-cols-5">
          <ExposeField label={t('Listen address')}><Input required value={a.address} onChange={(e) => setPolicy({ ...policy, allocations: policy.allocations.map((v, j) => j === i ? { ...v, address: e.target.value } : v) })} /></ExposeField>
          <ExposeField label={t('Protocol')}><select className={exposeSelectClass} value={a.protocol} onChange={(e) => setPolicy({ ...policy, allocations: policy.allocations.map((v, j) => j === i ? { ...v, protocol: e.target.value as ExposeProtocol } : v) })}><option value="tcp">TCP</option><option value="udp">UDP</option></select></ExposeField>
          <ExposeField label={t('First port')}><Input required type="number" min={1} max={65535} value={a.first_port} onChange={(e) => setPolicy({ ...policy, allocations: policy.allocations.map((v, j) => j === i ? { ...v, first_port: Number(e.target.value) } : v) })} /></ExposeField>
          <ExposeField label={t('Last port')}><Input required type="number" min={a.first_port} max={65535} value={a.last_port} onChange={(e) => setPolicy({ ...policy, allocations: policy.allocations.map((v, j) => j === i ? { ...v, last_port: Number(e.target.value) } : v) })} /></ExposeField>
          <Button type="button" variant="outline" onClick={() => setPolicy({ ...policy, allocations: policy.allocations.filter((_, j) => i !== j) })}>{t('Remove')}</Button>
        </div>)}
        <Button type="button" variant="outline" onClick={() => setPolicy({ ...policy, allocations: [...policy.allocations, { address: '0.0.0.0', protocol: 'tcp', first_port: 20000, last_port: 20099 }] })}>{t('Add port allocation')}</Button>
      </fieldset>
      <fieldset className="grid gap-3"><legend className="mb-2 text-sm font-medium">{t('Reserved listeners')}</legend>
        {policy.reserved.map((a, i) => <div key={i} className="grid grid-cols-2 items-end gap-2 rounded-lg border p-3 sm:grid-cols-4">
          <ExposeField label={t('Listen address')}><Input required value={a.address} onChange={(e) => setPolicy({ ...policy, reserved: policy.reserved.map((v, j) => j === i ? { ...v, address: e.target.value } : v) })} /></ExposeField>
          <ExposeField label={t('Protocol')}><select className={exposeSelectClass} value={a.protocol} onChange={(e) => setPolicy({ ...policy, reserved: policy.reserved.map((v, j) => j === i ? { ...v, protocol: e.target.value as ExposeProtocol } : v) })}><option value="tcp">TCP</option><option value="udp">UDP</option></select></ExposeField>
          <ExposeField label={t('Port')}><Input required type="number" min={1} max={65535} value={a.port} onChange={(e) => setPolicy({ ...policy, reserved: policy.reserved.map((v, j) => j === i ? { ...v, port: Number(e.target.value) } : v) })} /></ExposeField>
          <Button type="button" variant="outline" onClick={() => setPolicy({ ...policy, reserved: policy.reserved.filter((_, j) => i !== j) })}>{t('Remove')}</Button>
        </div>)}
        <Button type="button" variant="outline" onClick={() => setPolicy({ ...policy, reserved: [...policy.reserved, { address: '0.0.0.0', protocol: 'tcp', port: 20000 }] })}>{t('Reserve a listener')}</Button>
      </fieldset>
      <fieldset className="grid gap-3 sm:grid-cols-2"><legend className="mb-2 text-sm font-medium">{t('Organization totals')}</legend>
        {Object.entries(limits).map(([key, label]) => <ExposeField key={key} label={label}><Input required type="number" min={0} max={Number.MAX_SAFE_INTEGER} step={1} value={policy[key as keyof typeof limits]} onChange={(e) => setPolicy({ ...policy, [key]: Number(e.target.value) })} /></ExposeField>)}
      </fieldset>
      {error && <p role="alert" className="text-sm text-destructive">{error}</p>}
    </form>
    <DialogFooter><Button variant="outline" disabled={busy} onClick={onClose}>{t('Cancel')}</Button><Button type="submit" form={formId} disabled={busy}>{t('Save')}</Button></DialogFooter>
  </DialogContent></Dialog>
}

export function ExposePolicies({ policies, onSaved }: { policies: ExposePolicy[]; onSaved: () => void }) {
  const { t } = useI18n()
  const { masterAdmin, orgs, selectedOrg } = useSession()
  const orgLabel = useOrgLabel()
  const [org, setOrg] = useState(selectedOrg)
  const [editing, setEditing] = useState<ExposePolicy | null>(null)
  const add = () => setEditing(policies.find((p) => p.org_id === org) ?? { org_id: org, revision: 0, allocations: [], reserved: [], max_rules: 10, max_tcp_connections: 2560, max_udp_sessions: 10240, ingress_bytes_per_second: 167772160, egress_bytes_per_second: 167772160 })
  return <details className="rounded-lg border p-4"><summary className="cursor-pointer text-sm font-medium">{t('Port allocations and quotas')}</summary><div className="mt-3 grid gap-3">
    {policies.map((p) => <div key={p.org_id} className="grid gap-2 rounded-lg bg-muted p-3 text-sm"><div className="flex items-center justify-between"><strong>{orgLabel(p.org_id)}</strong>{masterAdmin && <Button size="xs" variant="outline" onClick={() => setEditing(p)}>{t('Edit')}</Button>}</div>
      {p.allocations.map((a, i) => <code key={i} className="text-xs">{a.protocol} {a.address} : {a.first_port}–{a.last_port}</code>)}
      <p>{t('Rule quota')}: {p.max_rules} · TCP: {p.max_tcp_connections} · UDP: {p.max_udp_sessions}</p>
      <p>{t('Ingress limit (bytes/s)')}: {p.ingress_bytes_per_second} · {t('Egress limit (bytes/s)')}: {p.egress_bytes_per_second}</p>
      {p.reserved.length > 0 && <p>{t('Reserved listeners')}: {p.reserved.map((a) => `${a.protocol} ${a.address}:${a.port}`).join(', ')}</p>}
    </div>)}
    {masterAdmin && <div className="flex flex-wrap items-end gap-2"><ExposeField label={t('Organization')}><select className={exposeSelectClass} value={org} onChange={(e) => setOrg(e.target.value)}>{orgs.filter((o) => o.id !== '*').map((o) => <option key={o.id} value={o.id}>{orgLabel(o.id)}</option>)}</select></ExposeField><Button size="sm" onClick={add}>{t('Set port allocation')}</Button></div>}
    {editing && <PolicyEditor initial={editing} onClose={() => setEditing(null)} onSaved={onSaved} />}
  </div></details>
}

/** Reuses the same structured bounds form without mutating the server policy.
 * The enclosing user/key form commits the identity and its restriction together. */
export function ExposeBoundsEditor({ org, value, onChange }: { org: string; value: ExposePolicy | null; onChange: (policy: ExposePolicy | null) => void }) {
  const { t } = useI18n()
  const { orgs, masterAdmin } = useSession()
  const held = orgs.find((o) => o.id === org)
  const canDelegate = masterAdmin || !!held?.expose_actions?.includes('delegate')
  const { data: policies } = useStream(null, exposeApi.policies, 30000)
  const [editing, setEditing] = useState<ExposePolicy | null>(null)
  const parent = held?.expose_bounds
  const seed = () => structuredClone(value ?? parent ?? policies?.find((p) => p.org_id === org) ?? {
    org_id: org, revision: 1, allocations: [], reserved: [], max_rules: 0, max_tcp_connections: 0,
    max_udp_sessions: 0, ingress_bytes_per_second: 0, egress_bytes_per_second: 0,
  })
  if (org === '*') return null
  return <fieldset className="grid gap-2 rounded-lg border p-3"><legend className="px-1 text-sm font-medium">{t('Delegated expose limits')}</legend>
    <p className="text-xs text-muted-foreground">{t('These limits restrict this identity. Organization-owned listeners keep their lifetime when a user loses access.')}</p>
    {value ? <div className="grid gap-1 text-xs">
      {value.allocations.map((a, i) => <code key={i}>{a.protocol} {a.address} : {a.first_port}–{a.last_port}</code>)}
      <p>{t('Rule quota')}: {value.max_rules} · TCP: {value.max_tcp_connections} · UDP: {value.max_udp_sessions}</p>
      <p>{t('Ingress limit (bytes/s)')}: {value.ingress_bytes_per_second} · {t('Egress limit (bytes/s)')}: {value.egress_bytes_per_second}</p>
    </div> : <p className="text-xs">{t('Only the organization policy applies; no additional identity limit is set.')}</p>}
    {canDelegate && <div className="flex gap-2"><Button type="button" size="xs" variant="outline" onClick={() => setEditing(seed())}>{t('Edit')}</Button>
      {value && !parent && <Button type="button" size="xs" variant="outline" onClick={() => onChange(null)}>{t('Remove')}</Button>}
    </div>}
    {parent && <p className="text-xs text-muted-foreground">{t('Delegated limits must remain within your own limits.')}</p>}
    {editing && <PolicyEditor initial={editing} commit={(p) => onChange({ ...p, org_id: org, revision: 1 })} onClose={() => setEditing(null)} onSaved={() => {}} />}
  </fieldset>
}
