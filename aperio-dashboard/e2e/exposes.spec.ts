import { expect, test, type Page } from '@playwright/test'
import type { ExposeAction, ExposeItem, ExposePolicy, ExposeSpec } from '../src/lib/api'

// Browser contract tests use a deterministic API. Real sockets, persistence
// and authorization are covered separately by server acceptance tests.
const actions: ExposeAction[] = ['read', 'create', 'update', 'enable', 'disable', 'delete', 'disconnect']
const policy: ExposePolicy = {
  org_id: 'acme-id', revision: 1,
  allocations: ['tcp', 'udp'].map((protocol) => ({ address: '127.0.0.1', protocol: protocol as 'tcp' | 'udp', first_port: 20000, last_port: 20010 })),
  reserved: [], max_rules: 10, max_tcp_connections: 1024, max_udp_sessions: 4096,
  ingress_bytes_per_second: 100000000, egress_bytes_per_second: 100000000,
}

async function dashboard(page: Page, permitted = true) {
  const records = new Map<string, ExposeItem>()
  const state = { conflict: false, rejectedField: false }
  await page.route('**/aperio/api/**', async (route) => {
    const request = route.request()
    const path = new URL(request.url()).pathname.replace('/aperio/api', '')
    const reply = (body: unknown, status = 200) => route.fulfill({ status, json: body })
    if (path === '/session') return reply({ username: 'publisher', role: 'viewer', totp: false, expires_in_seconds: 3600,
      master_admin: false, all_orgs: false, selected_org: 'acme-id',
      orgs: [{ id: 'acme-id', name: 'acme', custom_name: 'Acme', role: 'viewer', expose_actions: permitted ? actions : [], expose_bounds: policy }] })
    if (path === '/tunnels') return reply([])
    if (path === '/exposes/policies') return reply(permitted ? [policy] : [])
    if (path === '/exposes' && request.method() === 'GET') return reply({ items: [...records.values()], total: records.size, volatile: false })
    if (path === '/exposes' && request.method() === 'POST') {
      if (!permitted) return reply({ code: 'forbidden', message: 'No expose permission' }, 403)
      const body = request.postDataJSON() as { id: string; spec: ExposeSpec }
      if (state.rejectedField) { state.rejectedField = false; return reply({ code: 'validation', errors: [{ field: 'listener.address', message: 'Use an allocated address' }] }, 400) }
      records.set(body.id, { resource: { id: body.id, revision: 1, source: 'api', spec: body.spec,
        state: body.spec.enabled ? 'listening' : 'disabled', target_state: 'waiting', served_by: null, error: null,
        sessions: 0, up_bytes: 0, down_bytes: 0, up_packets: 0, down_packets: 0, drops: {} }, actions, policy, bounds: policy })
      return reply(records.get(body.id)!.resource, 201)
    }
    const id = path.split('/')[2]
    const item = records.get(id)
    if (item && path.endsWith('/actions')) {
      const body = request.postDataJSON()
      item.resource.revision += 1
      item.resource.spec.enabled = body.action === 'enable'
      item.resource.state = body.action === 'enable' ? 'listening' : 'disabled'
      return reply(item.resource)
    }
    if (item && request.method() === 'PUT') {
      if (state.conflict) { state.conflict = false; item.resource.revision += 1; return reply({ code: 'revision_conflict', message: 'Reload this revision' }, 409) }
      const body = request.postDataJSON()
      expect(body.revision).toBe(item.resource.revision)
      item.resource.spec = body.spec; item.resource.revision += 1
      return reply(item.resource)
    }
    if (item && request.method() === 'DELETE') { records.delete(id); return reply({ ok: true }) }
    if (item && (path.endsWith('/sessions') || path.endsWith('/events'))) return reply({ items: [], total: 0 })
    if (item) return reply(item.resource)
    // Unrelated overview data is deliberately unavailable: this also exercises
    // the tunnel workspace's independence from an overview request failure.
    return reply({ message: 'Not available in this fixture' }, 503)
  })
  await page.goto('/aperio/?tab=tunnels')
  await expect(page.getByRole('region', { name: 'Public exposes' })).toBeVisible()
  return { records, state }
}

for (const mobile of [false, true]) {
  test(`delegated viewer publishes TCP and UDP and operates them${mobile ? ' on mobile' : ''}`, async ({ page }) => {
    if (mobile) await page.setViewportSize({ width: 390, height: 844 })
    const { records } = await dashboard(page)
    for (const protocol of ['tcp', 'udp']) {
      await page.getByRole('button', { name: 'Create public expose', exact: true }).click()
      const dialog = page.getByRole('dialog')
      await dialog.getByLabel('Tunnel name', { exact: true }).fill(`echo_${protocol}`)
      await dialog.getByLabel('Protocol', { exact: true }).selectOption(protocol)
      await dialog.getByLabel('Listen address', { exact: true }).fill('127.0.0.1')
      await dialog.getByLabel('Port', { exact: true }).fill('20000')
      await dialog.getByLabel('Enabled', { exact: true }).check()
      await dialog.getByLabel('I understand that this endpoint accepts public traffic.').check()
      await dialog.getByRole('button', { name: 'Save', exact: true }).click()
      await expect(dialog).toBeHidden()
      const row = page.getByRole('row').filter({ hasText: `echo_${protocol}` })
      await expect(row).toContainText('Listening')
      await expect(row).toContainText('Waiting for target')
      await row.getByRole('button', { name: 'Disable', exact: true }).click()
      await page.getByRole('dialog').getByRole('button', { name: 'Disable', exact: true }).click()
      await expect(row).toContainText('Disabled')
      await row.getByRole('button', { name: 'Delete', exact: true }).click()
      await page.getByRole('dialog').getByRole('button', { name: 'Delete', exact: true }).click()
      await expect(row).toHaveCount(0)
    }
    expect(records.size).toBe(0)
  })
}

test('field errors are associated and stale edits preserve the draft', async ({ page }) => {
  const { state, records } = await dashboard(page)
  state.rejectedField = true
  await page.getByRole('button', { name: 'Create public expose', exact: true }).click()
  const dialog = page.getByRole('dialog')
  await dialog.getByLabel('Tunnel name', { exact: true }).fill('original')
  await dialog.getByRole('button', { name: 'Save', exact: true }).click()
  await expect(dialog.getByLabel('Listen address', { exact: true })).toHaveAttribute('aria-invalid', 'true')
  await dialog.getByLabel('Listen address', { exact: true }).fill('127.0.0.1')
  await dialog.getByRole('button', { name: 'Save', exact: true }).click()
  await expect(dialog).toBeHidden()
  state.conflict = true
  await page.getByRole('row').filter({ hasText: 'original' }).getByRole('button', { name: 'Edit', exact: true }).click()
  await dialog.getByLabel('Tunnel name', { exact: true }).fill('my_draft')
  await dialog.getByRole('button', { name: 'Save', exact: true }).click()
  await expect(dialog.getByLabel('Tunnel name', { exact: true })).toHaveValue('my_draft')
  await dialog.getByRole('button', { name: 'Load current version', exact: true }).click()
  await dialog.getByRole('button', { name: 'Keep my draft against this version', exact: true }).click()
  await dialog.getByRole('button', { name: 'Save', exact: true }).click()
  await expect(dialog).toBeHidden()
  expect([...records.values()][0].resource.spec.tunnel).toBe('my_draft')
})

test('same-role user without expose permission has no publish control', async ({ page }) => {
  await dashboard(page, false)
  await expect(page.getByRole('button', { name: 'Create public expose', exact: true })).toHaveCount(0)
  await expect(page.getByText('No public exposes match your access and filters.')).toBeVisible()
})
