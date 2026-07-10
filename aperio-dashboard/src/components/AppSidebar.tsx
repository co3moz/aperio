import {
  ActivityIcon,
  GlobeIcon,
  KeyRoundIcon,
  LayoutDashboardIcon,
  LogOutIcon,
  Settings2Icon,
} from 'lucide-react'
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
import { formatUptime } from '@/lib/format'

export type Page = 'overview' | 'traffic' | 'access' | 'system'

export const PAGES: { id: Page; label: string; icon: typeof GlobeIcon; hint: string }[] = [
  { id: 'overview', label: 'Overview', icon: LayoutDashboardIcon, hint: 'Stats & clients' },
  { id: 'traffic', label: 'Traffic', icon: ActivityIcon, hint: 'Live requests' },
  { id: 'access', label: 'Access', icon: KeyRoundIcon, hint: 'Tokens & sharing' },
  { id: 'system', label: 'System', icon: Settings2Icon, hint: 'Settings & audit' },
]

export function AppSidebar({
  page,
  onNavigate,
  sessionSeconds,
  version,
  onSignOut,
}: {
  page: Page
  onNavigate: (page: Page) => void
  sessionSeconds: number | null
  version: string | null
  onSignOut: () => void
}) {
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
        <SidebarGroup>
          <SidebarGroupLabel>Panel</SidebarGroupLabel>
          <SidebarGroupContent>
            <SidebarMenu>
              {PAGES.map((p) => (
                <SidebarMenuItem key={p.id}>
                  <SidebarMenuButton
                    tooltip={p.label}
                    isActive={page === p.id}
                    onClick={() => onNavigate(p.id)}
                  >
                    <p.icon />
                    <span>{p.label}</span>
                  </SidebarMenuButton>
                </SidebarMenuItem>
              ))}
            </SidebarMenu>
          </SidebarGroupContent>
        </SidebarGroup>
      </SidebarContent>
      <SidebarFooter>
        <SidebarMenu>
          <SidebarMenuItem>
            <SidebarMenuButton
              tooltip={
                sessionSeconds != null
                  ? `Session expires in ${formatUptime(sessionSeconds)}`
                  : 'Sign out'
              }
              onClick={onSignOut}
            >
              <LogOutIcon />
              <span className="flex-1">Sign out</span>
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
