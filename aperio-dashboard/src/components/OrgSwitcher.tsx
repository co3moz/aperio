import { Building2Icon, CheckIcon, ChevronsUpDownIcon } from 'lucide-react'
import { useState } from 'react'
import { toast } from 'sonner'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import {
  SidebarMenu,
  SidebarMenuButton,
  SidebarMenuItem,
} from '@/components/ui/sidebar'
import { usePoll } from '@/hooks/usePoll'
import { useI18n } from '@/i18n'
import { api, ApiError } from '@/lib/api'

/** Organization picker for the master super-admin. Switching stores the choice
 *  on the session server-side, then reloads so every section re-fetches the
 *  newly-scoped clients, tokens, and users. */
export function OrgSwitcher({ selectedOrg }: { selectedOrg: string }) {
  const { t } = useI18n()
  const { data: orgs } = usePoll(api.orgs, 30_000)
  const [busy, setBusy] = useState(false)

  const current = orgs?.find((o) => o.id === selectedOrg)
  const currentName = current?.name ?? (selectedOrg === 'master' ? t('master') : selectedOrg)

  const switchTo = async (id: string) => {
    if (id === selectedOrg || busy) return
    setBusy(true)
    try {
      await api.selectOrg(id)
      // Reload so all data-fetching sections re-run under the new org scope.
      window.location.reload()
    } catch (e) {
      setBusy(false)
      toast.error(e instanceof ApiError ? e.message : String(e))
    }
  }

  return (
    <SidebarMenu>
      <SidebarMenuItem>
        <DropdownMenu>
          <DropdownMenuTrigger
            render={
              <SidebarMenuButton
                size="lg"
                tooltip={t('Organization')}
                className="data-[state=open]:bg-sidebar-accent"
              />
            }
          >
            <div className="flex size-6 shrink-0 items-center justify-center rounded-md border bg-sidebar-accent text-sidebar-accent-foreground">
              <Building2Icon className="size-3.5" />
            </div>
            <div className="grid flex-1 text-left leading-tight">
              <span className="text-[10px] uppercase tracking-wider text-muted-foreground">
                {t('Organization')}
              </span>
              <span className="truncate text-sm font-medium">{currentName}</span>
            </div>
            <ChevronsUpDownIcon className="ml-auto size-4 opacity-60" />
          </DropdownMenuTrigger>
          <DropdownMenuContent align="start" className="min-w-56">
            <div className="px-3 py-2 text-xs text-muted-foreground">
              {t('Switch organization')}
            </div>
            <DropdownMenuSeparator />
            {(orgs ?? []).map((o) => (
              <DropdownMenuItem key={o.id} onClick={() => void switchTo(o.id)}>
                <Building2Icon className="size-4 opacity-70" />
                <span className="flex-1 truncate">{o.name}</span>
                <span className="text-xs text-muted-foreground">
                  {o.users}·{o.tokens}
                </span>
                {o.id === selectedOrg && <CheckIcon className="size-4" />}
              </DropdownMenuItem>
            ))}
          </DropdownMenuContent>
        </DropdownMenu>
      </SidebarMenuItem>
    </SidebarMenu>
  )
}
