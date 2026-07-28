import { Building2Icon, InboxIcon, Settings2Icon, UsersIcon, WebhookIcon } from 'lucide-react'
import { AdminKeysSection } from './AdminKeysSection'
import { InboxSection } from './InboxSection'
import { OrganizationsSection } from './OrganizationsSection'
import { SettingsSection } from './SettingsSection'
import { UsersSection } from './UsersSection'
import { WebhooksSection } from './WebhooksSection'
import { PaneDialog, type PaneSpec } from './PaneDialog'
import { useI18n } from '@/i18n'
import type { Role } from '@/lib/api'
import type { PaneFocus } from '@/lib/paneFocus'
import type { Page } from './AppSidebar'

/**
 * The pages that configure the server.
 *
 * You open a setting, change it, and leave, which is what a dialog is for.
 * Webhooks and the webhook inbox come here because what you do with them is
 * configure and act on them, even though both carry a table of past
 * deliveries. The pages that only report — traffic, clients, breakdown — stay
 * full screen, and the ones that exist to diagnose live in Tools.
 */
export const SETTINGS_PAGES = [
  'settings',
  'organizations',
  'users',
  'webhooks',
  'inbox',
] as const

export type SettingsPage = (typeof SETTINGS_PAGES)[number]

export function isSettingsPage(page: Page): page is SettingsPage {
  return (SETTINGS_PAGES as readonly string[]).includes(page)
}

/**
 * The panes, with the same reach rules the pages carried before they moved
 * here. Kept beside them rather than derived from the sidebar's page list:
 * the sidebar now has a single entry for all of them, so it no longer knows
 * who may see which.
 */
export const SETTINGS_PANES: PaneSpec<SettingsPage>[] = [
  { id: 'settings', label: 'Server Settings', icon: Settings2Icon, minRole: 'admin', masterOnly: true },
  { id: 'organizations', label: 'Organizations', icon: Building2Icon, minRole: 'admin', masterOnly: true },
  { id: 'users', label: 'Users', icon: UsersIcon, minRole: 'admin' },
  { id: 'webhooks', label: 'Webhooks', icon: WebhookIcon, minRole: 'viewer' },
  { id: 'inbox', label: 'Webhook Inbox', icon: InboxIcon, minRole: 'viewer' },
]

export function SettingsDialog({
  page,
  role,
  masterAdmin,
  focus,
  onNavigate,
  onClose,
}: {
  page: SettingsPage
  role: Role
  /** The built-in super-admin, the only one who sees the master-only panes. */
  masterAdmin: boolean
  focus?: PaneFocus | null
  onNavigate: (page: SettingsPage) => void
  onClose: () => void
}) {
  const { t } = useI18n()
  return (
    <PaneDialog
      page={page}
      panes={SETTINGS_PANES}
      role={role}
      masterAdmin={masterAdmin}
      focus={focus}
      title={t('Settings')}
      description={t('Configure this server, its organizations and who may sign in.')}
      onNavigate={onNavigate}
      onClose={onClose}
    >
      {(current) => (
        <>
          {current === 'settings' && <SettingsSection />}
          {current === 'organizations' && <OrganizationsSection />}
          {current === 'users' && (
            <div className="space-y-6">
              <UsersSection />
              <AdminKeysSection />
            </div>
          )}
          {current === 'webhooks' && <WebhooksSection />}
          {current === 'inbox' && <InboxSection />}
        </>
      )}
    </PaneDialog>
  )
}
