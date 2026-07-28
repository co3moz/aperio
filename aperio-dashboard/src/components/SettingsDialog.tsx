import { Building2Icon, Settings2Icon, UsersIcon } from 'lucide-react'
import { AdminKeysSection } from './AdminKeysSection'
import { OrganizationsSection } from './OrganizationsSection'
import { SettingsSection } from './SettingsSection'
import { UsersSection } from './UsersSection'
import { Dialog, DialogContent, DialogDescription, DialogTitle } from '@/components/ui/dialog'
import {
  Sidebar,
  SidebarContent,
  SidebarGroup,
  SidebarGroupContent,
  SidebarMenu,
  SidebarMenuButton,
  SidebarMenuItem,
  SidebarProvider,
} from '@/components/ui/sidebar'
import { useI18n } from '@/i18n'
import type { Role } from '@/lib/api'
import { ROLE_ORDER, type Page } from './AppSidebar'

/**
 * The pages that configure the server, as opposed to the ones that report on
 * it.
 *
 * The distinction is what decides the shape: you go *to* a log or an explorer
 * and stay there, so those keep the whole window; you open a setting, change
 * it, and leave, which is a dialog. Webhooks, the audit log, the API explorer
 * and the config builder stay full screen for that reason, the first because
 * half of it is a delivery log and the rest because they are wide by nature.
 */
export const SETTINGS_PAGES = ['settings', 'organizations', 'users'] as const

export type SettingsPage = (typeof SETTINGS_PAGES)[number]

export function isSettingsPage(page: Page): page is SettingsPage {
  return (SETTINGS_PAGES as readonly string[]).includes(page)
}

/**
 * The panes, with the same reach rules the pages carried before they moved
 * here. Kept beside them rather than derived from the sidebar's page list:
 * the sidebar now has a single entry for all three, so it no longer knows
 * who may see which.
 */
const PANES: {
  id: SettingsPage
  label: string
  icon: typeof Settings2Icon
  minRole: Role
  masterOnly?: boolean
}[] = [
  { id: 'settings', label: 'Server Settings', icon: Settings2Icon, minRole: 'admin', masterOnly: true },
  { id: 'organizations', label: 'Organizations', icon: Building2Icon, minRole: 'admin', masterOnly: true },
  { id: 'users', label: 'Users', icon: UsersIcon, minRole: 'admin' },
]

/**
 * The settings pages, in one dialog with their own nav down the side.
 *
 * They stay real pages underneath: the URL still names which pane is open, so
 * a link to a setting still lands on it and the command palette still reaches
 * them. The dialog is how they are presented, not what they are.
 */
export function SettingsDialog({
  page,
  role,
  masterAdmin,
  onNavigate,
  onClose,
}: {
  page: SettingsPage
  role: Role
  /** The built-in super-admin, the only one who sees the master-only panes. */
  masterAdmin: boolean
  onNavigate: (page: SettingsPage) => void
  onClose: () => void
}) {
  const { t } = useI18n()
  const panes = PANES.filter(
    (p) => ROLE_ORDER[role] >= ROLE_ORDER[p.minRole] && (!p.masterOnly || masterAdmin),
  )
  const current = panes.find((p) => p.id === page) ?? panes[0]

  return (
    <Dialog open onOpenChange={(open) => !open && onClose()}>
      <DialogContent className="overflow-hidden p-0 sm:max-w-4xl">
        <DialogTitle className="sr-only">{t('Settings')}</DialogTitle>
        <DialogDescription className="sr-only">
          {t('Configure this server, its organizations and who may sign in.')}
        </DialogDescription>
        <SidebarProvider className="min-h-0 items-start">
          <Sidebar collapsible="none" className="hidden w-52 shrink-0 bg-transparent md:flex">
            <SidebarContent>
              <SidebarGroup>
                <SidebarGroupContent>
                  <SidebarMenu>
                    {panes.map((pane) => (
                      <SidebarMenuItem key={pane.id}>
                        <SidebarMenuButton
                          isActive={pane.id === current?.id}
                          onClick={() => onNavigate(pane.id)}
                        >
                          <pane.icon />
                          <span>{t(pane.label)}</span>
                        </SidebarMenuButton>
                      </SidebarMenuItem>
                    ))}
                  </SidebarMenu>
                </SidebarGroupContent>
              </SidebarGroup>
            </SidebarContent>
          </Sidebar>
          {/* The pane scrolls, not the dialog: the nav beside it has to stay
              put, or choosing the next setting means scrolling back up. */}
          <main className="flex h-[70dvh] min-w-0 flex-1 flex-col overflow-y-auto p-6">
            <h2 className="mb-4 font-heading text-base font-medium md:hidden">
              {current && t(current.label)}
            </h2>
            {page === 'settings' && <SettingsSection />}
            {page === 'organizations' && <OrganizationsSection />}
            {page === 'users' && (
              <div className="space-y-6">
                <UsersSection />
                <AdminKeysSection />
              </div>
            )}
          </main>
        </SidebarProvider>
      </DialogContent>
    </Dialog>
  )
}
