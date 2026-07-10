import { MoonIcon, SearchIcon, SunIcon, TriangleAlertIcon } from 'lucide-react'
import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { AppSidebar, PAGES, type Page } from './components/AppSidebar'
import { ActivityChart } from './components/ActivityChart'
import { AuditSection } from './components/AuditSection'
import { ClientsSection } from './components/ClientsSection'
import { CommandPalette, type Command } from './components/CommandPalette'
import { InspectorDialog } from './components/InspectorDialog'
import { MaintenanceSection } from './components/MaintenanceSection'
import { SettingsSection } from './components/SettingsSection'
import { ShareLinksSection } from './components/ShareLinksSection'
import { StatsCards } from './components/StatsCards'
import { TokensSection } from './components/TokensSection'
import { TrafficBreakdownSection } from './components/TrafficBreakdownSection'
import { TrafficSection } from './components/TrafficSection'
import { WebhooksSection } from './components/WebhooksSection'
import { StatusDot } from './components/shared'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Separator } from '@/components/ui/separator'
import { SidebarInset, SidebarProvider, SidebarTrigger } from '@/components/ui/sidebar'
import { Tooltip, TooltipContent, TooltipTrigger } from '@/components/ui/tooltip'
import { useLiveData } from './hooks/useLiveData'
import { usePoll } from './hooks/usePoll'
import { api, logout } from './lib/api'
import { formatUptime } from './lib/format'
import { readParams, writeParams } from './lib/url'
import { useThemeMode } from './theme'
import { cn } from '@/lib/utils'

const POLL_INTERVAL_MS = 2000
const HISTORY_LENGTH = 30
const HISTORY_KEY = 'aperio-activity-history'
// Restore the sparkline only if the saved sample is recent, so a tab reopened
// much later starts clean instead of replaying stale bars.
const HISTORY_MAX_AGE_MS = 15_000

function isPage(value: string | null): value is Page {
  return PAGES.some((p) => p.id === value)
}

// Old bookmarks used the four coarse tabs; land them on the closest new page.
const LEGACY_TABS: Record<string, Page> = { access: 'tokens', system: 'settings' }

function pageFromUrl(): Page {
  const t = readParams().get('tab')
  if (isPage(t)) return t
  return (t && LEGACY_TABS[t]) || 'overview'
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
  const { data: session } = usePoll(api.session, 60_000)
  // The server version only changes on restart; a slow poll keeps it honest.
  const { data: health } = usePoll(api.health, 300_000)
  const [inspectId, setInspectId] = useState<string | null>(null)
  const [page, setPage] = useState<Page>(pageFromUrl)
  const [paletteOpen, setPaletteOpen] = useState(false)
  const { appearance, toggle } = useThemeMode()

  // Navigate tabs through the URL so reloads/bookmarks land on the same tab and
  // the browser back button steps between them.
  const goto = useCallback((next: Page) => {
    setPage(next)
    const params = readParams()
    params.set('tab', next)
    writeParams(params, true)
  }, [])
  useEffect(() => {
    const onPop = () => setPage(pageFromUrl())
    window.addEventListener('popstate', onPop)
    return () => window.removeEventListener('popstate', onPop)
  }, [])

  // Requests/second sparkline derived from the total_requests delta between
  // consecutive stats polls, persisted so a reload keeps recent history.
  const [history, setHistory] = useState<number[]>(loadHistory)
  const lastTotal = useRef<number | null>(null)
  useEffect(() => {
    if (!stats) return
    if (lastTotal.current === null) {
      lastTotal.current = stats.total_requests
      return
    }
    const diff = stats.total_requests - lastTotal.current
    lastTotal.current = stats.total_requests
    setHistory((h) => [...h.slice(1), Math.max(diff / (POLL_INTERVAL_MS / 1000), 0)])
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
    const color = connected ? '#84cc16' : statsError ? '#ef4444' : '#f59e0b'
    const svg = `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16"><circle cx="8" cy="8" r="7" fill="${color}"/></svg>`
    let link = document.querySelector<HTMLLinkElement>("link[rel='icon']")
    if (!link) {
      link = document.createElement('link')
      link.rel = 'icon'
      document.head.appendChild(link)
    }
    link.type = 'image/svg+xml'
    link.href = `data:image/svg+xml,${encodeURIComponent(svg)}`
  }, [connected, statsError])

  const signOut = useCallback(async () => {
    await logout()
    window.location.assign('/aperio/auth')
  }, [])

  const commands = useMemo<Command[]>(
    () => [
      ...PAGES.map((p) => ({
        id: `nav-${p.id}`,
        label: `Go to ${p.label}`,
        hint: 'Navigate',
        icon: p.icon,
        run: () => goto(p.id),
      })),
      {
        id: 'toggle-theme',
        label: `Switch to ${appearance === 'dark' ? 'light' : 'dark'} theme`,
        hint: 'Appearance',
        icon: appearance === 'dark' ? SunIcon : MoonIcon,
        run: toggle,
      },
      { id: 'sign-out', label: 'Sign out', hint: 'Session', run: () => void signOut() },
    ],
    [appearance, toggle, goto, signOut],
  )

  const active = PAGES.find((p) => p.id === page) ?? PAGES[0]

  return (
    <SidebarProvider>
      <AppSidebar
        page={page}
        onNavigate={goto}
        sessionSeconds={session?.expires_in_seconds ?? null}
        version={health?.version ?? null}
        onSignOut={() => void signOut()}
      />
      <SidebarInset>
        <header className="sticky top-0 z-10 flex h-14 shrink-0 items-center gap-2 border-b bg-background/80 px-4 backdrop-blur">
          <SidebarTrigger className="-ml-1" />
          <Separator orientation="vertical" className="mr-1 !h-4" />
          <div className="flex min-w-0 flex-col">
            <span className="font-heading truncate text-sm font-semibold leading-tight">
              {active.label}
            </span>
            <span className="hidden truncate text-xs text-muted-foreground sm:block">
              {active.hint}
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
              <span className="hidden sm:inline">Search…</span>
              <kbd className="pointer-events-none hidden rounded-md border bg-muted px-1.5 font-mono text-[10px] font-medium text-muted-foreground sm:inline-block">
                ⌘K
              </kbd>
            </Button>
            <Tooltip>
              <TooltipTrigger
                render={
                  <Button
                    variant="ghost"
                    size="icon-sm"
                    onClick={toggle}
                    aria-label="Toggle color theme"
                  />
                }
              >
                {appearance === 'dark' ? <SunIcon /> : <MoonIcon />}
              </TooltipTrigger>
              <TooltipContent>
                Switch to {appearance === 'dark' ? 'light' : 'dark'} theme
              </TooltipContent>
            </Tooltip>
            <Badge
              variant="outline"
              className={cn(
                'gap-1.5 rounded-full px-2.5 py-1',
                connected ? 'text-emerald-600 dark:text-emerald-400' : 'text-red-600 dark:text-red-400',
              )}
            >
              <StatusDot active={connected} />
              <span className="hidden md:inline">
                {connected ? 'Connected & Active' : 'Offline (Waiting for Client)'}
              </span>
              <span className="md:hidden">{connected ? 'Online' : 'Offline'}</span>
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
                ? "Dashboard data isn't updating — the values shown may be stale."
                : 'Cannot reach the server. Retrying automatically…'}
            </div>
          )}
          {page === 'overview' && (
            <>
              <StatsCards stats={stats} />
              <ActivityChart history={history} />
            </>
          )}
          {page === 'clients' && (
            <ClientsSection clients={stats?.active_clients ?? []} onChanged={refreshStats} />
          )}
          {page === 'traffic' && <TrafficSection logs={logs} onInspect={setInspectId} />}
          {page === 'breakdown' && <TrafficBreakdownSection stats={stats} />}
          {page === 'tokens' && <TokensSection />}
          {page === 'share' && <ShareLinksSection />}
          {page === 'maintenance' && <MaintenanceSection />}
          {page === 'settings' && <SettingsSection />}
          {page === 'webhooks' && <WebhooksSection />}
          {page === 'audit' && <AuditSection />}
        </main>

        <footer className="border-t py-3 text-center text-xs text-muted-foreground">
          Aperio Reverse Tunneling System • Server Uptime: {formatUptime(stats?.uptime_seconds ?? 0)}
        </footer>
      </SidebarInset>

      <CommandPalette open={paletteOpen} onOpenChange={setPaletteOpen} commands={commands} />
      <InspectorDialog id={inspectId} onClose={() => setInspectId(null)} />
    </SidebarProvider>
  )
}
