import {
  ActivityIcon,
  CableIcon,
  ChartPieIcon,
  ConstructionIcon,
  GaugeIcon,
  GlobeIcon,
  KeyRoundIcon,
  LayoutDashboardIcon,
  Link2Icon,
  LogOutIcon,
  ChevronsUpDownIcon,
  ServerIcon,
  UserRoundIcon,
  Settings2Icon,
  FingerprintIcon,
  ShieldCheckIcon,
  WaypointsIcon,
  WrenchIcon,
} from 'lucide-react'
import { AperioMark } from './AperioMark'
import { AperioWordmark } from './AperioWordmark'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import { OrgSwitcher } from './OrgSwitcher'
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
  SidebarRail,
} from '@/components/ui/sidebar'
import { useI18n } from '@/i18n'
import type { Role } from '@/lib/api'
import { formatUptime } from '@/lib/format'

export type Page =
  | 'overview'
  | 'clients'
  | 'tunnels'
  | 'traffic'
  | 'inbox'
  | 'breakdown'
  | 'topology'
  | 'tokens'
  | 'share'
  | 'maintenance'
  | 'scaling'
  | 'settings'
  | 'export'
  | 'webhooks'
  | 'messages'
  | 'audit'
  | 'users'
  | 'organizations'
  | 'api'
  | 'config-builder'

export interface PageSpec {
  id: Page
  label: string
  icon: typeof GlobeIcon
  hint: string
  /** The article under docs/ for this page, linked from the page header. */
  docs?: string
  /** Minimum role that may see/open this page (default: viewer). */
  minRole?: Role
  /** Only visible to the built-in `aperio` super-admin (organization mgmt). */
  masterOnly?: boolean
}

export const PAGE_GROUPS: { label: string; pages: PageSpec[] }[] = [
  {
    label: 'Overview',
    pages: [
      { id: 'overview', label: 'Overview', icon: LayoutDashboardIcon, hint: 'Stats & live activity', docs: 'dashboard.md#live-overview' },
      { id: 'clients', label: 'Clients', icon: ServerIcon, hint: 'Active tunnel connections', docs: 'dashboard.md#clients-table' },
      { id: 'tunnels', label: 'Tunnels', icon: CableIcon, hint: 'Private services reachable with --bind-tunnels', docs: 'emergency-tunnels.md' },
    ],
  },
  {
    label: 'Traffic',
    pages: [
      { id: 'traffic', label: 'Live Traffic', icon: ActivityIcon, hint: 'Requests in real time, table or console', docs: 'dashboard.md#live-traffic-table' },
      { id: 'breakdown', label: 'Breakdown', icon: ChartPieIcon, hint: 'Traffic by token & hostname', docs: 'observability.md#persistent-statistics' },
      { id: 'topology', label: 'Topology', icon: WaypointsIcon, hint: 'Routes, clients & backends as a live map', docs: 'dashboard.md#topology' },
    ],
  },
  {
    label: 'Access',
    pages: [
      { id: 'tokens', label: 'API Tokens', icon: KeyRoundIcon, hint: 'Scoped tunnel credentials', docs: 'tokens-and-auth.md#dynamic-tokens' },
      { id: 'share', label: 'Share Links', icon: Link2Icon, hint: 'Temporary visitor access', docs: 'share-links.md' },
      { id: 'maintenance', label: 'Maintenance', icon: ConstructionIcon, hint: 'Per-hostname 503 switch', docs: 'dashboard.md#maintenance-mode' },
      { id: 'scaling', label: 'Autoscaling', icon: GaugeIcon, hint: 'Cold start & scale-out records', docs: 'autoscaling.md' },
    ],
  },
  {
    label: 'System',
    pages: [
      // Two entries for eight pages: each opens as a pane of a dialog rather
      // than as a full-screen page, so the sidebar should not read as if they
      // were eight destinations. The pages themselves still exist, which is
      // what keeps their links working.
      { id: 'settings', label: 'Settings', icon: Settings2Icon, hint: 'Server, organizations and users', docs: 'dashboard.md#settings-dialog', minRole: 'admin' },
      { id: 'audit', label: 'Tools', icon: WrenchIcon, hint: 'Audit log, API explorer and config builder', docs: 'dashboard.md#tools' },
    ],
  },
]

export const PAGES: PageSpec[] = PAGE_GROUPS.flatMap((g) => g.pages)

export const ROLE_ORDER: Record<Role, number> = { viewer: 0, operator: 1, admin: 2 }

/** Pages the given role may access. Master-only pages (organization
 *  management) are visible solely to the built-in `aperio` super-admin. */
export function pagesForRole(role: Role, masterAdmin = false): PageSpec[] {
  return PAGES.filter(
    (p) => ROLE_ORDER[role] >= ROLE_ORDER[p.minRole ?? 'viewer'] && (!p.masterOnly || masterAdmin),
  )
}

export function AppSidebar({
  page,
  onNavigate,
  username,
  sessionSeconds,
  version,
  role,
  masterAdmin,
  selectedOrg,
  reachableOrgs,
  onSignOut,
  onOpenTotp,
  onOpenPasskeys,
}: {
  page: Page
  onNavigate: (page: Page) => void
  /** Signed-in identity, shown in the footer entry. */
  username: string
  sessionSeconds: number | null
  version: string | null
  role: Role
  masterAdmin: boolean
  selectedOrg: string
  /** How many organizations a grant reaches; the picker appears past one. */
  reachableOrgs: number
  onSignOut: () => void
  onOpenTotp: () => void
  onOpenPasskeys: () => void
}) {
  const { t } = useI18n()
  const order = ROLE_ORDER[role]
  return (
    <Sidebar collapsible="icon">
      <SidebarHeader>
        <SidebarMenu>
          <SidebarMenuItem>
            <SidebarMenuButton size="lg" className="pointer-events-none">
              {/* No tile behind the mark: it stands on the sidebar itself and
                  takes `--sidebar-foreground`, which is near-black in the light
                  theme and near-white in the dark one. Inherited rather than
                  named, so it stays right if the palette moves. */}
              <div className="flex size-8 shrink-0 items-center justify-center">
                {/* `!` because SidebarMenuButton's base carries `[&_svg]:size-4`,
                    a descendant selector that outranks a plain size utility on
                    specificity and pins every nested icon to 16px. */}
                <AperioMark className="size-[30px]!" />
              </div>
              <div className="grid flex-1 text-left leading-tight">
                <AperioWordmark className="truncate text-[15px] font-normal" />
                <span className="truncate text-xs text-muted-foreground">
                  {version ? `v${version}` : '…'}
                </span>
              </div>
            </SidebarMenuButton>
          </SidebarMenuItem>
        </SidebarMenu>
        {(masterAdmin || reachableOrgs > 1) && <OrgSwitcher selectedOrg={selectedOrg} />}
      </SidebarHeader>
      <SidebarContent>
        {PAGE_GROUPS.map((group) => {
          const pages = group.pages.filter(
            (p) => order >= ROLE_ORDER[p.minRole ?? 'viewer'] && (!p.masterOnly || masterAdmin),
          )
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
            {/* The footer is where the signed-in identity lives, with its
                actions behind it: three flat buttons competed with the
                navigation above them for the same visual weight, when none of
                them is a place you go. */}
            <DropdownMenu>
              <DropdownMenuTrigger
                render={
                  <SidebarMenuButton
                    size="lg"
                    tooltip={username}
                    className="data-[state=open]:bg-sidebar-accent"
                  />
                }
              >
                <div className="flex size-8 shrink-0 items-center justify-center rounded-lg bg-sidebar-accent text-sidebar-accent-foreground">
                  <UserRoundIcon className="size-4" />
                </div>
                <div className="grid flex-1 text-left leading-tight">
                  <span className="truncate text-sm font-medium">{username}</span>
                  <span className="truncate text-xs text-muted-foreground">
                    {role === 'admin'
                      ? t('Admin')
                      : role === 'operator'
                        ? t('Operator')
                        : t('Viewer')}
                  </span>
                </div>
                <ChevronsUpDownIcon className="ml-auto size-4 opacity-60" />
              </DropdownMenuTrigger>
              <DropdownMenuContent align="end" side="top" className="min-w-56">
                <div className="px-3 py-2 text-xs text-muted-foreground">
                  {sessionSeconds != null
                    ? t('Session expires in {duration}', {
                        duration: formatUptime(sessionSeconds),
                      })
                    : username}
                </div>
                <DropdownMenuSeparator />
                <DropdownMenuItem onClick={onOpenTotp}>
                  <ShieldCheckIcon className="size-4 opacity-70" />
                  <span className="flex-1">{t('Two-factor auth')}</span>
                </DropdownMenuItem>
                <DropdownMenuItem onClick={onOpenPasskeys}>
                  <FingerprintIcon className="size-4 opacity-70" />
                  <span className="flex-1">{t('Passkeys')}</span>
                </DropdownMenuItem>
                <DropdownMenuSeparator />
                <DropdownMenuItem onClick={onSignOut}>
                  <LogOutIcon className="size-4 opacity-70" />
                  <span className="flex-1">{t('Sign out')}</span>
                </DropdownMenuItem>
              </DropdownMenuContent>
            </DropdownMenu>
          </SidebarMenuItem>
        </SidebarMenu>
      </SidebarFooter>
      {/* The edge rail: a click target along the whole border for collapsing
          the sidebar, so the toggle is not only the one button in the page
          header. */}
      <SidebarRail />
    </Sidebar>
  )
}
