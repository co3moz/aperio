import type { LucideIcon } from 'lucide-react'
import { useCallback, useState, type ReactNode } from 'react'
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
  SidebarHeader,
  SidebarMenu,
  SidebarMenuButton,
  SidebarMenuItem,
  SidebarProvider,
} from '@/components/ui/sidebar'
import { useI18n } from '@/i18n'
import type { Role } from '@/lib/api'
import { UnsavedContext } from '@/lib/unsaved'
import { PaneFocusContext, type PaneFocus } from '@/lib/paneFocus'
import { ROLE_ORDER } from './AppSidebar'
import { cn } from '@/lib/utils'

export interface PaneSpec<Id extends string> {
  id: Id
  label: string
  icon: LucideIcon
  /** Minimum role that may open this pane. */
  minRole: Role
  /** Only the built-in `aperio` super-admin sees it. */
  masterOnly?: boolean
}

/**
 * A group of pages presented as one dialog with its own nav down the side.
 *
 * Deliberately absent from the URL. A dialog is something you open on top of
 * what you were doing, and routing it made it replace that instead: the page
 * underneath was lost, the back button stepped through panes, and a reload
 * came back into a screen nobody asked to be on. So the page under it keeps
 * the URL, and a reload returns to it with the dialog closed, which is only
 * safe because a pane holding unsaved edits says so and gets a confirmation
 * before it is thrown away.
 *
 * Shared by Settings and Tools rather than copied: the two differ in which
 * panes they list and how much width those panes want, and nothing else.
 */
export function PaneDialog<Id extends string>({
  page,
  panes,
  role,
  masterAdmin,
  focus,
  title,
  description,
  className,
  onNavigate,
  onClose,
  children,
}: {
  page: Id
  panes: PaneSpec<Id>[]
  role: Role
  masterAdmin: boolean
  /** Something finer than a pane to reveal on open, see `PaneFocus`. */
  focus?: PaneFocus | null
  /** Names the dialog for screen readers; the panes carry the visible titles. */
  title: string
  description: string
  /** Width cap, when the panes need more room than the default. */
  className?: string
  onNavigate: (page: Id) => void
  onClose: () => void
  children: (page: Id) => ReactNode
}) {
  const { t } = useI18n()
  const visible = panes.filter(
    (p) => ROLE_ORDER[role] >= ROLE_ORDER[p.minRole] && (!p.masterOnly || masterAdmin),
  )
  // The pane actually shown. A link (or a stale one) can name a pane this
  // role may not open, and falling back to the first *visible* one is the
  // only answer that is not a blank panel, the nav highlighted the fallback
  // while the body rendered nothing, because it was still asked for the pane
  // that was refused.
  const current = visible.find((p) => p.id === page) ?? visible[0]

  const [dirty, setDirty] = useState(false)
  // The exit a confirmation is currently standing in front of: a pane to move
  // to, or `null` for closing the dialog. `undefined` = nothing is being asked.
  const [pending, setPending] = useState<Id | null | undefined>(undefined)

  const go = useCallback(
    (to: Id | null) => (to === null ? onClose() : onNavigate(to)),
    [onClose, onNavigate],
  )
  // Switching panes unmounts the form just as surely as closing does, so both
  // ways out ask the same question.
  const leave = (to: Id | null) => (dirty ? setPending(to) : go(to))

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
      <DialogContent className={cn('overflow-hidden p-0 sm:max-w-4xl', className)}>
        {/* Visible in the nav above; still announced here for the small
            screens where the nav is not rendered at all. */}
        <DialogTitle className="sr-only">{title}</DialogTitle>
        <DialogDescription className="sr-only">{description}</DialogDescription>
        {/* `min-w-0`: this is a grid item, whose automatic minimum size is its
            content's, so without it the nav plus the widest thing in a pane
            set the width and the dialog's `overflow-hidden` simply cut off
            everything past its right edge, action buttons included. */}
        <SidebarProvider className="min-h-0 min-w-0 items-start">
          <Sidebar collapsible="none" className="hidden w-52 shrink-0 bg-transparent md:flex">
            <SidebarContent>
              {/* Which dialog this is. Settings and Tools are the same shape
                  with different panes, and opened from a command palette
                  there was nothing on screen saying which one you got. */}
              <SidebarHeader className="px-4 pt-4 pb-2">
                <span className="font-heading text-sm font-semibold tracking-tight">
                  {title}
                </span>
              </SidebarHeader>
              <SidebarGroup>
                <SidebarGroupContent>
                  <SidebarMenu>
                    {visible.map((pane) => (
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
              put, or choosing the next pane means scrolling back up. */}
          {/* `pr-14` clears the dialog's close button, which floats over the
              top-right corner: a pane whose own header carries an action put
              the two on top of each other. */}
          <main className="flex h-[70dvh] min-w-0 flex-1 flex-col overflow-y-auto p-6 pr-14">
            <h2 className="mb-4 font-heading text-base font-medium md:hidden">
              {current && t(current.label)}
            </h2>
            <PaneFocusContext.Provider value={focus ?? null}>
              <UnsavedContext.Provider value={setDirty}>
                {current && children(current.id)}
              </UnsavedContext.Provider>
            </PaneFocusContext.Provider>
          </main>
        </SidebarProvider>
      </DialogContent>
    </Dialog>
  )
}
