import {
  Building2Icon,
  MoonIcon,
  SearchIcon,
  Settings2Icon,
  SunIcon,
  TriangleAlertIcon,
  UserPlusIcon,
} from 'lucide-react'
import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { AppSidebar, PAGES, ROLE_ORDER, pagesForRole, type Page } from './components/AppSidebar'
import { AppearanceControls } from './components/AppearanceControls'
import {
  SETTINGS_PANES,
  SettingsDialog,
  isSettingsPage,
  type SettingsPage,
} from './components/SettingsDialog'
import { TOOLS_PANES, ToolsDialog, isToolsPage, type ToolsPage } from './components/ToolsDialog'
import { SETTING_FIELDS, type FieldSpec } from '@/lib/settingsCatalog'
import type { PaneFocus } from '@/lib/paneFocus'
import type { SettingsPayload } from './lib/api'
import { logoDataUri } from '@/lib/logo'
import { ActivityChart } from './components/ActivityChart'
import { ClientsSection } from './components/ClientsSection'
import { TunnelsSection } from './components/TunnelsSection'
import { UptimeSection } from './components/UptimeSection'
import { CommandPalette, type Command } from './components/CommandPalette'
import { InspectorDialog } from './components/InspectorDialog'
import { MaintenanceSection } from './components/MaintenanceSection'
import { ScalingSection } from './components/ScalingSection'
import { ShareLinksSection } from './components/ShareLinksSection'
import { StatsCards } from './components/StatsCards'
import { TokensSection } from './components/TokensSection'
import { TrafficBreakdownSection } from './components/TrafficBreakdownSection'
import { TopologySection } from './components/TopologySection'
import { StageStatsSection } from './components/StageStatsSection'
import { CacheStatsSection } from './components/CacheStatsSection'
import { SelfHealthSection } from './components/SelfHealthSection'
import { BandwidthSection } from './components/BandwidthSection'
import { RouteTrendsSection } from './components/RouteTrendsSection'
import { SlowEndpointsSection } from './components/SlowEndpointsSection'
import { TrafficSection } from './components/TrafficSection'
import { StatusDot } from './components/shared'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Separator } from '@/components/ui/separator'
import { SidebarInset, SidebarProvider, SidebarTrigger } from '@/components/ui/sidebar'
import { useLiveData } from './hooks/useLiveData'
import { usePoll } from './hooks/usePoll'
import { api, logout, type Role } from './lib/api'
import { formatBytes, formatUptime } from './lib/format'
import { readParams, writeParams } from './lib/url'
import { useThemeMode } from './theme'
import { useI18n } from '@/i18n'
import { SessionProvider } from '@/lib/session'
import { PasskeysDialog } from './components/PasskeysDialog'
import { TotpDialog } from './components/TotpDialog'
import { cn } from '@/lib/utils'

const HISTORY_LENGTH = 30
const HISTORY_KEY = 'aperio-activity-history'
// Restore the sparkline only if the saved sample is recent, so a tab reopened
// much later starts clean instead of replaying stale bars.
const HISTORY_MAX_AGE_MS = 15_000

/** A pane of the Settings or Tools dialog: a page, but never a destination. */
type OverlayPage = SettingsPage | ToolsPage

function isOverlayPage(page: Page): page is OverlayPage {
  return isSettingsPage(page) || isToolsPage(page)
}

function isPage(value: string | null): value is Page {
  // The dialog panes are pages the sidebar no longer lists individually, so
  // `PAGES` alone would refuse a perfectly good link to one.
  return PAGES.some((p) => p.id === value) || isOverlayPage(value as Page)
}

// Old bookmarks used the four coarse tabs; land them on the closest new page.
// `tail` was merged into the traffic view as its console toggle.
const LEGACY_TABS: Record<string, Page> = { access: 'tokens', system: 'settings', tail: 'traffic' }

/** What `?tab=` names, resolved through the legacy aliases. */
function tabFromUrl(): Page {
  const t = readParams().get('tab')
  if (isPage(t)) return t
  return (t && LEGACY_TABS[t]) || 'overview'
}

// The settings dialog is not a destination, so it never appears in the URL —
// but links written while it was one still exist. They open it once, over the
// overview, and the parameter is dropped on arrival so a reload does not put
// the reader back in a settings screen they never navigated to.
function pageFromUrl(): Page {
  const tab = tabFromUrl()
  return isOverlayPage(tab) ? 'overview' : tab
}

function paneFromUrl(): OverlayPage | null {
  const tab = tabFromUrl()
  return isOverlayPage(tab) ? tab : null
}

/**
 * Things the palette can do rather than places it can go: one step from
 * anywhere to the form that does it.
 */
const ACTIONS: {
  target: string
  pane: OverlayPage
  label: string
  icon: typeof Settings2Icon
  minRole: Role
  masterOnly?: boolean
}[] = [
  { target: 'new-user', pane: 'users', label: 'Add User', icon: UserPlusIcon, minRole: 'admin' },
  {
    target: 'new-org',
    pane: 'organizations',
    label: 'New Organization',
    icon: Building2Icon,
    minRole: 'admin',
    masterOnly: true,
  },
]

/**
 * What a setting is set to right now, as one short string.
 *
 * An override is what the server is actually running; the env default is what
 * it falls back to. Both are worth showing and they are not the same claim,
 * so an overridden value says so.
 */
function describeSetting(payload: SettingsPayload, field: FieldSpec): string {
  const override = payload.overrides[field.key]
  const value = override ?? payload.defaults[field.key]
  if (value === undefined || value === null || value === '') return '—'
  const text =
    typeof value === 'boolean'
      ? value
        ? 'on'
        : 'off'
      : field.kind === 'bytes'
        ? formatBytes(Number(value))
        : String(value)
  return override !== undefined && override !== null ? `${text} *` : text
}

function loadHistory(): number[] {
  try {
    const raw = localStorage.getItem(HISTORY_KEY)
    if (raw) {
      const saved = JSON.parse(raw) as { at: number; values: number[] }
      if (
        Array.isArray(saved.values) &&
        saved.values.length === HISTORY_LENGTH &&
        Date.now() - saved.at < HISTORY_MAX_AGE_MS
      ) {
        return saved.values
      }
    }
  } catch {
    // Corrupt or unavailable storage: start from an empty sparkline.
  }
  return new Array<number>(HISTORY_LENGTH).fill(0)
}

export default function App() {
  // Traffic and stats are pushed live over one SSE stream (with a polling
  // fallback if it drops); only the session lifetime is still polled.
  const { logs, stats, error: statsError, refreshStats } = useLiveData()
  const { data: session, refresh: refreshSession } = usePoll(api.session, 60_000)
  const [totpOpen, setTotpOpen] = useState(false)
  const [passkeysOpen, setPasskeysOpen] = useState(false)
  // The server version only changes on restart; a slow poll keeps it honest.
  const { data: health } = usePoll(api.health, 300_000)
  // Capture permalink: the inspected request id lives in the URL (?inspect=),
  // so an opened capture can be bookmarked or shared and reopens on reload.
  const [inspectId, setInspectId] = useState<string | null>(() => readParams().get('inspect'))
  const [page, setPage] = useState<Page>(pageFromUrl)
  const [overlay, setOverlay] = useState<OverlayPage | null>(paneFromUrl)
  // What the pane should reveal on arrival, and a counter so asking for the
  // same thing twice still counts as asking.
  const [focus, setFocus] = useState<PaneFocus | null>(null)
  const focusSeq = useRef(0)
  const [paletteOpen, setPaletteOpen] = useState(false)
  const { appearance, toggle } = useThemeMode()
  const { t } = useI18n()

  // Navigate tabs through the URL so reloads/bookmarks land on the same tab and
  // the browser back button steps between them. A settings pane is the one
  // thing that does not travel that way: it opens over whatever is on screen
  // and leaves the URL — and therefore the page underneath — untouched.
  const goto = useCallback((next: Page) => {
    if (isOverlayPage(next)) {
      setOverlay(next)
      return
    }
    setPage(next)
    const params = readParams()
    params.set('tab', next)
    writeParams(params, true)
  }, [])

  /** Opens a dialog pane, optionally on something particular inside it. */
  const openPane = useCallback((pane: OverlayPage, target?: string) => {
    setOverlay(pane)
    focusSeq.current += 1
    setFocus(target ? { target, seq: focusSeq.current } : null)
  }, [])

  // Drop a legacy `?tab=<pane>` once it has been honoured, so the dialog it
  // opened is not what a reload comes back to.
  useEffect(() => {
    if (paneFromUrl() === null) return
    const params = readParams()
    params.delete('tab')
    writeParams(params, true)
  }, [])

  useEffect(() => {
    const onPop = () => {
      setPage(pageFromUrl())
      setInspectId(readParams().get('inspect'))
    }
    window.addEventListener('popstate', onPop)
    return () => window.removeEventListener('popstate', onPop)
  }, [])

  // Open/close a capture, reflecting the id in the URL so it is shareable.
  const setInspect = useCallback((id: string | null) => {
    setInspectId(id)
    const params = readParams()
    if (id) params.set('inspect', id)
    else params.delete('inspect')
    writeParams(params, true)
  }, [])

  // Requests/second sparkline derived from the total_requests delta between
  // consecutive stats polls, persisted so a reload keeps recent history.
  const [history, setHistory] = useState<number[]>(loadHistory)
  const lastTotal = useRef<number | null>(null)
  const lastSampleAt = useRef<number | null>(null)
  useEffect(() => {
    if (!stats) return
    const now = Date.now()
    if (lastTotal.current === null || lastSampleAt.current === null) {
      lastTotal.current = stats.total_requests
      lastSampleAt.current = now
      return
    }
    const diff = stats.total_requests - lastTotal.current
    // Time the samples rather than assuming a fixed cadence: snapshots arrive
    // every two seconds over the live stream but every three while it is down
    // and the dashboard is polling, and dividing by a hardcoded two overstated
    // the rate by half whenever the fallback was in use.
    const elapsedSecs = Math.max((now - lastSampleAt.current) / 1000, 0.001)
    lastTotal.current = stats.total_requests
    lastSampleAt.current = now
    setHistory((h) => [...h.slice(1), Math.max(diff / elapsedSecs, 0)])
  }, [stats])

  useEffect(() => {
    try {
      localStorage.setItem(HISTORY_KEY, JSON.stringify({ at: Date.now(), values: history }))
    } catch {
      // Persisting the sparkline is best-effort.
    }
  }, [history])

  const connected = (stats?.connected_clients_count ?? 0) > 0

  // Reflect connection state in the tab title and favicon so a backgrounded tab
  // shows at a glance whether the tunnel is up.
  useEffect(() => {
    document.title = connected
      ? 'Aperio · Connected'
      : statsError
        ? 'Aperio · Disconnected'
        : 'Aperio · Waiting'
    // The mark, tinted by state: the tab still says at a glance whether the
    // tunnel is up, and now says which product it belongs to.
    const color = connected ? '#84cc16' : statsError ? '#ef4444' : '#f59e0b'
    let link = document.querySelector<HTMLLinkElement>("link[rel='icon']")
    if (!link) {
      link = document.createElement('link')
      link.rel = 'icon'
      document.head.appendChild(link)
    }
    link.type = 'image/svg+xml'
    link.href = logoDataUri(color)
  }, [connected, statsError])

  const signOut = useCallback(async () => {
    await logout()
    // Carry the dashboard as the post-login destination so signing back in
    // lands on /aperio again, not the login page's default ('/', the proxied
    // site root).
    window.location.assign('/aperio/auth?redirect=/aperio')
  }, [])

  // Until the session loads, assume the least privilege so admin-only pages
  // never flash; the server is the real gate regardless.
  const role = session?.role ?? 'viewer'
  const masterAdmin = session?.master_admin ?? false
  const selectedOrg = session?.selected_org ?? 'master'
  const allowedPages = useMemo(() => pagesForRole(role, masterAdmin), [role, masterAdmin])
  // The dialog panes this session may open, in the order the dialogs list them.
  const panes = useMemo(
    () =>
      [...SETTINGS_PANES, ...TOOLS_PANES].filter(
        (p) => ROLE_ORDER[role] >= ROLE_ORDER[p.minRole] && (!p.masterOnly || masterAdmin),
      ),
    [role, masterAdmin],
  )
  // Only the super-admin may read the settings, so only they get them in the
  // palette; for anyone else the request is a guaranteed 403.
  const [settingsPayload, setSettingsPayload] = useState<SettingsPayload | null>(null)
  useEffect(() => {
    if (!masterAdmin || role !== 'admin') return
    let live = true
    api
      .settings()
      .then((p) => live && setSettingsPayload(p))
      .catch(() => {})
    return () => {
      live = false
    }
  }, [masterAdmin, role])

  // A role that can't see the current page (e.g. a viewer landing on a
  // bookmarked ?tab=settings) is bounced to the overview.
  useEffect(() => {
    if (session && !allowedPages.some((p) => p.id === page)) goto('overview')
  }, [session, allowedPages, page, goto])

  const commands = useMemo<Command[]>(
    () => [
      // Full-screen destinations first: the sidebar's own list, in its order.
      ...allowedPages.map((p) => ({
        id: `nav-${p.id}`,
        label: t('Go to {page}', { page: t(p.label) }),
        hint: t('Navigate'),
        icon: p.icon,
        run: () => goto(p.id),
      })),
      {
        id: 'toggle-theme',
        label: appearance === 'dark' ? t('Switch to light theme') : t('Switch to dark theme'),
        hint: t('Appearance'),
        icon: appearance === 'dark' ? SunIcon : MoonIcon,
        run: toggle,
      },
      { id: 'sign-out', label: t('Sign out'), hint: t('Session'), run: () => void signOut() },

      // The dialog panes. They have no sidebar row of their own any more, so
      // without these the only way to Organizations is to open Settings and
      // know it is in there.
      ...panes.map((p) => ({
        id: `pane-${p.id}`,
        label: t('Go to {page}', { page: t(p.label) }),
        icon: p.icon,
        group: t('Settings & Tools'),
        run: () => openPane(p.id),
      })),
      ...ACTIONS.filter((a) => ROLE_ORDER[role] >= ROLE_ORDER[a.minRole] && (!a.masterOnly || masterAdmin)).map(
        (a) => ({
          id: `do-${a.target}`,
          label: t(a.label),
          icon: a.icon,
          group: t('Settings & Tools'),
          run: () => openPane(a.pane, a.target),
        }),
      ),

      // Every server setting by name, with what it is set to right now. The
      // value is the point: half the reason to look a setting up is to check
      // it, and that should not cost opening a dialog and a group to read one
      // number.
      ...(settingsPayload
        ? SETTING_FIELDS.map(({ field, group }) => ({
            id: `setting-${field.key}`,
            label: t(field.label),
            hint: t(group.title),
            detail: describeSetting(settingsPayload, field),
            keywords: `${field.key} ${group.title}`,
            icon: Settings2Icon,
            group: t('Server Settings'),
            run: () => openPane('settings', `setting:${field.key}`),
          }))
        : []),
    ],
    [
      allowedPages,
      appearance,
      toggle,
      goto,
      openPane,
      panes,
      role,
      masterAdmin,
      settingsPayload,
      signOut,
      t,
    ],
  )

  const active = PAGES.find((p) => p.id === page) ?? PAGES[0]
  // Render nothing role-restricted until the redirect effect settles.
  const canView = !session || allowedPages.some((p) => p.id === page)

  return (
    <SessionProvider
      username={session?.username ?? 'aperio'}
      role={role}
      selectedOrg={selectedOrg}
      masterAdmin={masterAdmin}
    >
    <SidebarProvider>
      <TotpDialog
        open={totpOpen}
        onOpenChange={setTotpOpen}
        enabled={session?.totp ?? false}
        onChanged={refreshSession}
      />
      <PasskeysDialog open={passkeysOpen} onOpenChange={setPasskeysOpen} />
      {overlay && isSettingsPage(overlay) && (
        <SettingsDialog
          page={overlay}
          role={role}
          masterAdmin={masterAdmin}
          focus={focus}
          onNavigate={openPane}
          onClose={() => setOverlay(null)}
        />
      )}
      {overlay && isToolsPage(overlay) && (
        <ToolsDialog
          page={overlay}
          role={role}
          masterAdmin={masterAdmin}
          focus={focus}
          onNavigate={openPane}
          onClose={() => setOverlay(null)}
        />
      )}
      <AppSidebar
        username={session?.username ?? 'aperio'}
        onOpenTotp={() => setTotpOpen(true)}
        onOpenPasskeys={() => setPasskeysOpen(true)}
        // With a dialog open, its sidebar entry is what is active, even though
        // the page under it has not changed. The Tools entry stands for three
        // panes and carries the id of the first, so the other two answer to it.
        page={overlay ? (isToolsPage(overlay) ? 'audit' : overlay) : page}
        onNavigate={goto}
        sessionSeconds={session?.expires_in_seconds ?? null}
        version={health?.version ?? null}
        role={role}
        masterAdmin={masterAdmin}
        selectedOrg={selectedOrg}
        onSignOut={() => void signOut()}
      />
      <SidebarInset>
        <header className="sticky top-0 z-10 flex h-14 shrink-0 items-center gap-2 border-b bg-background/80 px-4 backdrop-blur">
          <SidebarTrigger className="-ml-1" />
          <Separator orientation="vertical" className="mr-1 !h-4" />
          <div className="flex min-w-0 flex-col">
            <span className="font-heading truncate text-sm font-semibold leading-tight">
              {t(active.label)}
            </span>
            <span className="hidden truncate text-xs text-muted-foreground sm:block">
              {t(active.hint)}
            </span>
          </div>
          <div className="ml-auto flex items-center gap-2">
            <Button
              variant="outline"
              size="sm"
              className="text-muted-foreground"
              onClick={() => setPaletteOpen(true)}
            >
              <SearchIcon />
              <span className="hidden sm:inline">{t('Search…')}</span>
              <kbd className="pointer-events-none hidden rounded-md border bg-muted px-1.5 font-mono text-[10px] font-medium text-muted-foreground sm:inline-block">
                ⌘K
              </kbd>
            </Button>
            <AppearanceControls />
            {session && (
              <Badge variant="outline" className="hidden gap-1.5 rounded-full px-2.5 py-1 lg:inline-flex">
                <span className="text-muted-foreground">{session.username}</span>
                <span className="text-primary">{role === 'admin' ? t('Admin') : role === 'operator' ? t('Operator') : t('Viewer')}</span>
              </Badge>
            )}
            <Badge
              variant="outline"
              className={cn(
                'gap-1.5 rounded-full px-2.5 py-1',
                connected ? 'text-emerald-600 dark:text-emerald-400' : 'text-red-600 dark:text-red-400',
              )}
            >
              <StatusDot active={connected} />
              <span className="hidden md:inline">
                {connected ? t('Connected & Active') : t('Offline (Waiting for Client)')}
              </span>
              <span className="md:hidden">{connected ? t('Online') : t('Offline')}</span>
            </Badge>
          </div>
        </header>

        <main className="flex flex-1 flex-col gap-6 p-4 md:p-6">
          {statsError && (
            <div
              className={cn(
                'flex items-center gap-2 rounded-3xl border px-4 py-3 text-sm',
                stats
                  ? 'border-amber-500/30 bg-amber-500/10 text-amber-700 dark:text-amber-400'
                  : 'border-red-500/30 bg-red-500/10 text-red-700 dark:text-red-400',
              )}
            >
              <TriangleAlertIcon className="size-4 shrink-0" />
              {stats
                ? t("Dashboard data isn't updating — the values shown may be stale.")
                : t('Cannot reach the server. Retrying automatically…')}
            </div>
          )}
          {canView && (
            <>
              {page === 'overview' && (
                <>
                  <StatsCards stats={stats} />
                  <ActivityChart history={history} />
                </>
              )}
              {page === 'clients' && (
                <>
                  <ClientsSection clients={stats?.active_clients ?? []} onChanged={refreshStats} />
                  <UptimeSection />
                </>
              )}
              {page === 'tunnels' && <TunnelsSection />}
              {page === 'traffic' && <TrafficSection logs={logs} onInspect={setInspect} />}
              {page === 'breakdown' && (
                <div className="flex flex-col gap-6">
                  <TrafficBreakdownSection stats={stats} />
                  <RouteTrendsSection />
                  <BandwidthSection />
                  <SlowEndpointsSection />
                  <StageStatsSection />
                  <CacheStatsSection />
                  <SelfHealthSection />
                </div>
              )}
              {page === 'topology' && <TopologySection />}
              {page === 'tokens' && <TokensSection />}
              {page === 'share' && <ShareLinksSection />}
              {page === 'maintenance' && <MaintenanceSection />}
              {page === 'scaling' && <ScalingSection />}
            </>
          )}
        </main>

        <footer className="border-t py-3 text-center text-xs text-muted-foreground">
          {t('Aperio Reverse Tunneling System • Server Uptime: {uptime}', {
            uptime: formatUptime(stats?.uptime_seconds ?? 0),
          })}
        </footer>
      </SidebarInset>

      <CommandPalette open={paletteOpen} onOpenChange={setPaletteOpen} commands={commands} />
      <InspectorDialog id={inspectId} onClose={() => setInspect(null)} />
    </SidebarProvider>
    </SessionProvider>
  )
}
