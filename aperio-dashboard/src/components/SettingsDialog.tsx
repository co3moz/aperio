import { Building2Icon, InboxIcon, Settings2Icon, UsersIcon, WebhookIcon } from 'lucide-react'
import { useCallback, useState } from 'react'
import { AdminKeysSection } from './AdminKeysSection'
import { InboxSection } from './InboxSection'
import { OrganizationsSection } from './OrganizationsSection'
import { SettingsSection } from './SettingsSection'
import { UsersSection } from './UsersSection'
import { WebhooksSection } from './WebhooksSection'
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from '@/components/ui/alert-dialog'
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
import { UnsavedContext } from '@/lib/unsaved'
import { ROLE_ORDER, type Page } from './AppSidebar'

/**
 * The pages that configure the server, as opposed to the ones that report on
 * it.
 *
 * The distinction is what decides the shape: you go *to* a log or an explorer
 * and stay there, so those keep the whole window; you open a setting, change
 * it, and leave, which is a dialog. Webhooks and the webhook inbox come here
 * because what you do with them is configure and act on them, even though
 * both carry a table of past deliveries. The audit log, the API explorer and
 * the config builder stay full screen: they are read, not operated, and are
 * wide by nature.
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
  { id: 'webhooks', label: 'Webhooks', icon: WebhookIcon, minRole: 'viewer' },
  { id: 'inbox', label: 'Webhook Inbox', icon: InboxIcon, minRole: 'viewer' },
]

/**
 * The settings pages, in one dialog with their own nav down the side.
 *
 * Deliberately absent from the URL. A dialog is something you open on top of
 * what you were doing, and putting it in `?tab=` made it replace that instead:
 * the page underneath was lost, the back button stepped through settings panes,
 * and a reload came back into a settings screen nobody asked to be on. So the
 * page under it keeps the URL, and a reload returns to it with the dialog
 * closed — which is only safe because a pane holding unsaved edits says so, and
 * gets a confirmation before it is thrown away.
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

  const [dirty, setDirty] = useState(false)
  // The exit a confirmation is currently standing in front of: a pane to move
  // to, or `null` for closing the dialog. `undefined` = nothing is being asked.
  const [pending, setPending] = useState<SettingsPage | null | undefined>(undefined)

  const go = useCallback(
    (to: SettingsPage | null) => (to === null ? onClose() : onNavigate(to)),
    [onClose, onNavigate],
  )
  // Switching panes unmounts the form just as surely as closing does, so both
  // ways out ask the same question.
  const leave = (to: SettingsPage | null) => (dirty ? setPending(to) : go(to))

  return (
    <Dialog open onOpenChange={(open) => !open && leave(null)}>
      <AlertDialog
        open={pending !== undefined}
        onOpenChange={(open) => !open && setPending(undefined)}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>{t('Discard unsaved changes?')}</AlertDialogTitle>
            <AlertDialogDescription>
              {t('These settings were edited but never applied. Leaving now throws the edits away; the server keeps running what it has.')}
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>{t('Keep editing')}</AlertDialogCancel>
            <AlertDialogAction
              className="bg-destructive/10 text-destructive hover:bg-destructive/20"
              onClick={() => {
                const to = pending
                setPending(undefined)
                setDirty(false)
                if (to !== undefined) go(to)
              }}
            >
              {t('Discard')}
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
      <DialogContent className="overflow-hidden p-0 sm:max-w-4xl">
        <DialogTitle className="sr-only">{t('Settings')}</DialogTitle>
        <DialogDescription className="sr-only">
          {t('Configure this server, its organizations and who may sign in.')}
        </DialogDescription>
        {/* `min-w-0`: this is a grid item, whose automatic minimum size is its
            content's, so without it the nav plus the widest thing in the pane
            set the width and the dialog's `overflow-hidden` simply cut off
            everything past its right edge — action buttons included. */}
        <SidebarProvider className="min-h-0 min-w-0 items-start">
          <Sidebar collapsible="none" className="hidden w-52 shrink-0 bg-transparent md:flex">
            <SidebarContent>
              <SidebarGroup>
                <SidebarGroupContent>
                  <SidebarMenu>
                    {panes.map((pane) => (
                      <SidebarMenuItem key={pane.id}>
                        <SidebarMenuButton
                          isActive={pane.id === current?.id}
                          onClick={() => leave(pane.id)}
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
          {/* `pr-14` clears the dialog's close button, which floats over the
              top-right corner: a pane whose own header carries an action put
              the two on top of each other. */}
          <main className="flex h-[70dvh] min-w-0 flex-1 flex-col overflow-y-auto p-6 pr-14">
            <h2 className="mb-4 font-heading text-base font-medium md:hidden">
              {current && t(current.label)}
            </h2>
            <UnsavedContext.Provider value={setDirty}>
              {page === 'settings' && <SettingsSection />}
              {page === 'organizations' && <OrganizationsSection />}
              {page === 'users' && (
                <div className="space-y-6">
                  <UsersSection />
                  <AdminKeysSection />
                </div>
              )}
              {page === 'webhooks' && <WebhooksSection />}
              {page === 'inbox' && <InboxSection />}
            </UnsavedContext.Provider>
          </main>
        </SidebarProvider>
      </DialogContent>
    </Dialog>
  )
}
