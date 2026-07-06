import { GlobeIcon } from '@radix-ui/react-icons'
import { Badge, Box, Container, Flex, Heading, Separator, Text } from '@radix-ui/themes'
import { useEffect, useRef, useState } from 'react'
import { ActivityChart } from './components/ActivityChart'
import { AuditSection } from './components/AuditSection'
import { ClientsSection } from './components/ClientsSection'
import { InspectorDialog } from './components/InspectorDialog'
import { MaintenanceSection } from './components/MaintenanceSection'
import { StatsCards } from './components/StatsCards'
import { TokensSection } from './components/TokensSection'
import { TrafficSection } from './components/TrafficSection'
import { WebhooksSection } from './components/WebhooksSection'
import { usePoll } from './hooks/usePoll'
import { api } from './lib/api'
import { formatUptime } from './lib/format'

const POLL_INTERVAL_MS = 2000
const HISTORY_LENGTH = 30

export default function App() {
  const { data: stats, refresh: refreshStats } = usePoll(api.stats, POLL_INTERVAL_MS)
  const { data: logs } = usePoll(api.logs, POLL_INTERVAL_MS)
  const [inspectId, setInspectId] = useState<string | null>(null)

  // Requests/second sparkline derived from the total_requests delta between
  // consecutive stats polls.
  const [history, setHistory] = useState<number[]>(() => new Array<number>(HISTORY_LENGTH).fill(0))
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

  const connected = (stats?.connected_clients_count ?? 0) > 0

  return (
    <Flex direction="column" minHeight="100vh">
      <Box
        position="sticky"
        top="0"
        style={{
          zIndex: 10,
          backdropFilter: 'blur(12px)',
          backgroundColor: 'rgba(11, 15, 25, 0.8)',
          borderBottom: '1px solid var(--gray-a4)',
        }}
      >
        <Container size="4" px="5">
          <Flex justify="between" align="center" py="4">
            <Flex align="center" gap="2">
              <GlobeIcon width="22" height="22" color="var(--indigo-9)" />
              <Heading
                size="6"
                style={{
                  background: 'linear-gradient(135deg, #fff 0%, #a5b4fc 100%)',
                  WebkitBackgroundClip: 'text',
                  WebkitTextFillColor: 'transparent',
                }}
              >
                Aperio
              </Heading>
            </Flex>
            <Badge size="2" color={connected ? 'green' : 'red'} variant="surface" radius="full">
              <span className={`status-dot ${connected ? 'active' : 'inactive'}`} />
              {connected ? 'Connected & Active' : 'Offline (Waiting for Client)'}
            </Badge>
          </Flex>
        </Container>
      </Box>

      <Container size="4" px="5" flexGrow="1">
        <Flex direction="column" gap="6" py="6">
          <StatsCards stats={stats} />
          <ActivityChart history={history} />
          <ClientsSection clients={stats?.active_clients ?? []} onChanged={refreshStats} />
          <MaintenanceSection />
          <TokensSection />
          <WebhooksSection />
          <AuditSection />
          <TrafficSection logs={logs} onInspect={setInspectId} />
        </Flex>
      </Container>

      <Separator size="4" />
      <Flex justify="center" py="4">
        <Text size="1" color="gray">
          Aperio Reverse Tunneling System • Server Uptime: {formatUptime(stats?.uptime_seconds ?? 0)}
        </Text>
      </Flex>

      <InspectorDialog id={inspectId} onClose={() => setInspectId(null)} />
    </Flex>
  )
}
