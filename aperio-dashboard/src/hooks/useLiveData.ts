import { useCallback, useEffect, useState } from 'react'
import { api, type RequestLog, type ServerStats } from '../lib/api'

// Keep a bounded live window on the client so a long-lived stream can't grow
// unbounded; the traffic table only renders the newest slice anyway.
const MAX_LOGS = 500
// Fallback polling cadence used only while the SSE stream is unavailable.
const FALLBACK_POLL_MS = 3000

export interface LiveData {
  logs: RequestLog[] | null
  stats: ServerStats | null
  /** True while the live stream is down and the fallback poll is active. */
  error: boolean
  /** Force an immediate stats refetch (e.g. right after a dashboard mutation). */
  refreshStats: () => void
}

/**
 * Single live feed for the dashboard backed by the `/aperio/api/stream` SSE
 * endpoint: `traffic` events append to the request log, `stats` events replace
 * the stats snapshot (pushed every 2s and once on connect). Seeds from the REST
 * endpoints and, if the stream can't be established, transparently falls back to
 * polling both — so nothing goes stale.
 */
export function useLiveData(): LiveData {
  const [logs, setLogs] = useState<RequestLog[] | null>(null)
  const [stats, setStats] = useState<ServerStats | null>(null)
  const [error, setError] = useState(false)

  const refreshStats = useCallback(() => {
    api
      .stats()
      .then((s) => setStats(s))
      .catch(() => {
        // Best-effort; the stream or next poll will refresh it.
      })
  }, [])

  useEffect(() => {
    let cancelled = false
    let pollTimer: ReturnType<typeof setInterval> | undefined

    const seedLogs = () =>
      api
        .logs()
        .then((l) => {
          if (!cancelled) setLogs(l)
        })
        .catch(() => {})
    const seedStats = () =>
      api
        .stats()
        .then((s) => {
          if (!cancelled) setStats(s)
        })
        .catch(() => {})

    // Seed immediately so the view isn't empty until the first event arrives.
    void seedLogs()
    void seedStats()

    const startFallback = () => {
      if (pollTimer || cancelled) return
      setError(true)
      pollTimer = setInterval(() => {
        void seedLogs()
        void seedStats()
      }, FALLBACK_POLL_MS)
    }
    const stopFallback = () => {
      if (pollTimer) {
        clearInterval(pollTimer)
        pollTimer = undefined
      }
      setError(false)
    }

    const es = new EventSource('/aperio/api/stream')
    es.onopen = () => stopFallback()
    es.addEventListener('traffic', (e) => {
      try {
        const log = JSON.parse((e as MessageEvent).data) as RequestLog
        setLogs((cur) => {
          const next = [...(cur ?? []), log]
          return next.length > MAX_LOGS ? next.slice(-MAX_LOGS) : next
        })
      } catch {
        // Ignore malformed frames.
      }
    })
    es.addEventListener('stats', (e) => {
      try {
        setStats(JSON.parse((e as MessageEvent).data) as ServerStats)
      } catch {
        // Ignore malformed frames.
      }
    })
    es.onerror = () => startFallback()

    return () => {
      cancelled = true
      es.close()
      stopFallback()
    }
  }, [])

  return { logs, stats, error, refreshStats }
}
