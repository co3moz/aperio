import {
  ExclamationTriangleIcon,
  ExitIcon,
  GlobeIcon,
  MagnifyingGlassIcon,
  MoonIcon,
  SunIcon,
} from '@radix-ui/react-icons'
import {
  Badge,
  Box,
  Button,
  Callout,
  Container,
  Flex,
  Heading,
  IconButton,
  Separator,
  TabNav,
  Text,
  Tooltip,
} from '@radix-ui/themes'
import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
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
import { usePoll } from './hooks/usePoll'
import { api, logout } from './lib/api'
import { formatUptime } from './lib/format'
import { readParams, writeParams } from './lib/url'
import { useThemeMode } from './theme'

const POLL_INTERVAL_MS = 2000
const HISTORY_LENGTH = 30
const HISTORY_KEY = 'aperio-activity-history'
// Restore the sparkline only if the saved sample is recent, so a tab reopened
// much later starts clean instead of replaying stale bars.
const HISTORY_MAX_AGE_MS = 15_000

type Page = 'overview' | 'traffic' | 'access' | 'system'

const PAGES: { id: Page; label: string }[] = [
  { id: 'overview', label: 'Overview' },
  { id: 'traffic', label: 'Traffic' },
  { id: 'access', label: 'Access' },
  { id: 'system', label: 'System' },
]

function isPage(value: string | null): value is Page {
  return PAGES.some((p) => p.id === value)
}

function pageFromUrl(): Page {
  const t = readParams().get('tab')
  return isPage(t) ? t : 'overview'
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
  const { data: stats, refresh: refreshStats, error: statsError } = usePoll(api.stats, POLL_INTERVAL_MS)
  const { data: logs } = usePoll(api.logs, POLL_INTERVAL_MS)
  const { data: session } = usePoll(api.session, 60_000)
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
    const color = connected ? '#30a46c' : statsError ? '#e5484d' : '#f5a623'
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
        run: () => goto(p.id),
      })),
      {
        id: 'toggle-theme',
        label: `Switch to ${appearance === 'dark' ? 'light' : 'dark'} theme`,
        hint: 'Appearance',
        run: toggle,
      },
      { id: 'sign-out', label: 'Sign out', hint: 'Session', run: () => void signOut() },
    ],
    [appearance, toggle, goto, signOut],
  )

  return (
    <Flex direction="column" minHeight="100vh">
      <Box position="sticky" top="0" className="app-header">
        <Container size="4" px="5">
          <Flex justify="between" align="center" pt="4" pb="2">
            <Flex align="center" gap="2">
              <GlobeIcon width="22" height="22" color="var(--indigo-9)" />
              <Heading
                size="6"
                style={{
                  background: 'linear-gradient(135deg, var(--gray-12), var(--indigo-11))',
                  WebkitBackgroundClip: 'text',
                  WebkitTextFillColor: 'transparent',
                }}
              >
                Aperio
              </Heading>
            </Flex>
            <Flex align="center" gap="3">
              <Tooltip content="Command menu (⌘K / Ctrl+K)">
                <IconButton
                  size="2"
                  variant="ghost"
                  color="gray"
                  onClick={() => setPaletteOpen(true)}
                  aria-label="Open command menu"
                >
                  <MagnifyingGlassIcon />
                </IconButton>
              </Tooltip>
              <Tooltip content={`Switch to ${appearance === 'dark' ? 'light' : 'dark'} theme`}>
                <IconButton
                  size="2"
                  variant="ghost"
                  color="gray"
                  onClick={toggle}
                  aria-label="Toggle color theme"
                >
                  {appearance === 'dark' ? <SunIcon /> : <MoonIcon />}
                </IconButton>
              </Tooltip>
              <Badge size="2" color={connected ? 'green' : 'red'} variant="surface" radius="full">
                <span className={`status-dot ${connected ? 'active' : 'inactive'}`} />
                {connected ? 'Connected & Active' : 'Offline (Waiting for Client)'}
              </Badge>
              <Tooltip
                content={
                  session
                    ? `Session expires in ${formatUptime(session.expires_in_seconds)}`
                    : 'Sign out'
                }
              >
                <Button size="1" variant="soft" color="gray" onClick={() => void signOut()}>
                  <ExitIcon /> Sign out
                </Button>
              </Tooltip>
            </Flex>
          </Flex>
          <TabNav.Root>
            {PAGES.map((p) => (
              <TabNav.Link
                key={p.id}
                href={`?tab=${p.id}`}
                active={page === p.id}
                onClick={(e) => {
                  e.preventDefault()
                  goto(p.id)
                }}
              >
                {p.label}
              </TabNav.Link>
            ))}
          </TabNav.Root>
        </Container>
      </Box>

      <Container size="4" px="5" flexGrow="1">
        <Flex direction="column" gap="6" py="6">
          {statsError && (
            <Callout.Root color={stats ? 'amber' : 'red'} size="1">
              <Callout.Icon>
                <ExclamationTriangleIcon />
              </Callout.Icon>
              <Callout.Text>
                {stats
                  ? "Dashboard data isn't updating — the values shown may be stale."
                  : 'Cannot reach the server. Retrying automatically…'}
              </Callout.Text>
            </Callout.Root>
          )}
          {page === 'overview' && (
            <>
              <StatsCards stats={stats} />
              <ActivityChart history={history} />
              <ClientsSection clients={stats?.active_clients ?? []} onChanged={refreshStats} />
            </>
          )}
          {page === 'traffic' && (
            <>
              <TrafficSection logs={logs} onInspect={setInspectId} />
              <TrafficBreakdownSection stats={stats} />
            </>
          )}
          {page === 'access' && (
            <>
              <TokensSection />
              <ShareLinksSection />
              <MaintenanceSection />
            </>
          )}
          {page === 'system' && (
            <>
              <SettingsSection />
              <WebhooksSection />
              <AuditSection />
            </>
          )}
        </Flex>
      </Container>

      <Separator size="4" />
      <Flex justify="center" py="4">
        <Text size="1" color="gray">
          Aperio Reverse Tunneling System • Server Uptime: {formatUptime(stats?.uptime_seconds ?? 0)}
        </Text>
      </Flex>

      <CommandPalette open={paletteOpen} onOpenChange={setPaletteOpen} commands={commands} />
      <InspectorDialog id={inspectId} onClose={() => setInspectId(null)} />
    </Flex>
  )
}
