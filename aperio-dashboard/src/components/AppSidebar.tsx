import {
  ActivityIcon,
  ChartPieIcon,
  ConstructionIcon,
  GlobeIcon,
  KeyRoundIcon,
  LayoutDashboardIcon,
  Link2Icon,
  LogOutIcon,
  ScrollTextIcon,
  ServerIcon,
  Settings2Icon,
  WebhookIcon,
} from 'lucide-react'
import { UsersIcon } from 'lucide-react'
import {
  Sidebar,
  SidebarContent,
  SidebarFooter,
  SidebarGroup,
  SidebarGroupContent,
  SidebarGroupLabel,
  SidebarHeader,
  SidebarMenu,
  SidebarMenuButton,
  SidebarMenuItem,
} from '@/components/ui/sidebar'
import { useI18n } from '@/i18n'
import type { Role } from '@/lib/api'
import { formatUptime } from '@/lib/format'

export type Page =
  | 'overview'
  | 'clients'
  | 'traffic'
  | 'breakdown'
  | 'tokens'
  | 'share'
  | 'maintenance'
  | 'settings'
  | 'webhooks'
  | 'audit'
  | 'users'

export interface PageSpec {
  id: Page
  label: string
  icon: typeof GlobeIcon
  hint: string
  /** Minimum role that may see/open this page (default: viewer). */
  minRole?: Role
}

export const PAGE_GROUPS: { label: string; pages: PageSpec[] }[] = [
  {
    label: 'Overview',
    pages: [
      { id: 'overview', label: 'Overview', icon: LayoutDashboardIcon, hint: 'Stats & live activity' },
      { id: 'clients', label: 'Clients', icon: ServerIcon, hint: 'Active tunnel connections' },
    ],
  },
  {
    label: 'Traffic',
    pages: [
      { id: 'traffic', label: 'Live Traffic', icon: ActivityIcon, hint: 'Requests in real time' },
      { id: 'breakdown', label: 'Breakdown', icon: ChartPieIcon, hint: 'Traffic by token & hostname' },
    ],
  },
  {
    label: 'Access',
    pages: [
      { id: 'tokens', label: 'API Tokens', icon: KeyRoundIcon, hint: 'Scoped tunnel credentials' },
      { id: 'share', label: 'Share Links', icon: Link2Icon, hint: 'Temporary visitor access' },
      { id: 'maintenance', label: 'Maintenance', icon: ConstructionIcon, hint: 'Per-hostname 503 switch' },
    ],
  },
  {
    label: 'System',
    pages: [
      { id: 'settings', label: 'Server Settings', icon: Settings2Icon, hint: 'Runtime configuration', minRole: 'admin' },
      { id: 'users', label: 'Users', icon: UsersIcon, hint: 'Dashboard access & roles', minRole: 'admin' },
      { id: 'webhooks', label: 'Webhooks', icon: WebhookIcon, hint: 'Event deliveries' },
      { id: 'audit', label: 'Audit Log', icon: ScrollTextIcon, hint: 'Administrative events' },
    ],
  },
]

export const PAGES: PageSpec[] = PAGE_GROUPS.flatMap((g) => g.pages)

const ROLE_ORDER: Record<Role, number> = { viewer: 0, operator: 1, admin: 2 }

/** Pages the given role may access. */
export function pagesForRole(role: Role): PageSpec[] {
  return PAGES.filter((p) => ROLE_ORDER[role] >= ROLE_ORDER[p.minRole ?? 'viewer'])
}

export function AppSidebar({
  page,
  onNavigate,
  sessionSeconds,
  version,
  role,
  onSignOut,
}: {
  page: Page
  onNavigate: (page: Page) => void
  sessionSeconds: number | null
  version: string | null
  role: Role
  onSignOut: () => void
}) {
  const { t } = useI18n()
  const order = ROLE_ORDER[role]
  return (
    <Sidebar collapsible="icon">
      <SidebarHeader>
        <SidebarMenu>
          <SidebarMenuItem>
            <SidebarMenuButton size="lg" className="pointer-events-none">
              <div className="flex size-8 shrink-0 items-center justify-center rounded-2xl bg-primary text-primary-foreground">
                <GlobeIcon className="size-4" />
              </div>
              <div className="grid flex-1 text-left leading-tight">
                <span className="font-heading truncate font-semibold">Aperio</span>
                <span className="truncate text-xs text-muted-foreground">
                  {version ? `v${version}` : '…'}
                </span>
              </div>
            </SidebarMenuButton>
          </SidebarMenuItem>
        </SidebarMenu>
      </SidebarHeader>
      <SidebarContent>
        {PAGE_GROUPS.map((group) => {
          const pages = group.pages.filter((p) => order >= ROLE_ORDER[p.minRole ?? 'viewer'])
          if (pages.length === 0) return null
          return (
            <SidebarGroup key={group.label}>
              <SidebarGroupLabel>{t(group.label)}</SidebarGroupLabel>
              <SidebarGroupContent>
                <SidebarMenu>
                  {pages.map((p) => (
                    <SidebarMenuItem key={p.id}>
                      <SidebarMenuButton
                        tooltip={t(p.label)}
                        isActive={page === p.id}
                        onClick={() => onNavigate(p.id)}
                      >
                        <p.icon />
                        <span>{t(p.label)}</span>
                      </SidebarMenuButton>
                    </SidebarMenuItem>
                  ))}
                </SidebarMenu>
              </SidebarGroupContent>
            </SidebarGroup>
          )
        })}
      </SidebarContent>
      <SidebarFooter>
        <SidebarMenu>
          <SidebarMenuItem>
            <SidebarMenuButton
              tooltip={
                sessionSeconds != null
                  ? t('Session expires in {duration}', { duration: formatUptime(sessionSeconds) })
                  : t('Sign out')
              }
              onClick={onSignOut}
            >
              <LogOutIcon />
              <span className="flex-1">{t('Sign out')}</span>
              {sessionSeconds != null && (
                <span className="text-xs text-muted-foreground">
                  {formatUptime(sessionSeconds)}
                </span>
              )}
            </SidebarMenuButton>
          </SidebarMenuItem>
        </SidebarMenu>
      </SidebarFooter>
    </Sidebar>
  )
}
