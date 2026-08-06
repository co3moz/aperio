import { useCallback, useEffect, useState } from 'react'
import { api, type RequestLog, type ServerStats } from '../lib/api'
import type { ServerNotification } from '../lib/notifications'

// Keep a bounded live window on the client so a long-lived stream can't grow
// unbounded; the traffic table only renders the newest slice anyway.
const MAX_LOGS = 500
// The bell keeps far fewer: it is a list somebody reads, and what falls off
// the end of it is in the audit log, which is the record.
const MAX_NOTIFICATIONS = 50
// Fallback polling cadence used only while the SSE stream is unavailable.
const FALLBACK_POLL_MS = 3000

export interface LiveData {
  logs: RequestLog[] | null
  stats: ServerStats | null
  /** Server events for the notification bell, newest last. */
  notifications: ServerNotification[]
  /** True while the live stream is down and the fallback poll is active. */
  error: boolean
  /** Force an immediate stats refetch (e.g. right after a dashboard mutation). */
  refreshStats: () => void
}

/**
 * Single live feed for the dashboard backed by the `/aperio/api/stream` SSE
 * endpoint: `traffic` events append to the request log, `stats` events replace
 * the stats snapshot (pushed every 2s and once on connect), and `notification`
 * events accumulate for the bell. Seeds from the REST endpoints and, if the
 * stream can't be established, transparently falls back to polling both, so
 * nothing goes stale.
 *
 * Notifications have no polling fallback and no seed, deliberately: they are a
 * live signal, and the record of what happened while the tab was closed is the
 * audit log, which has its own screen and its own retention.
 */
export function useLiveData(): LiveData {
  const [logs, setLogs] = useState<RequestLog[] | null>(null)
  const [stats, setStats] = useState<ServerStats | null>(null)
  const [notifications, setNotifications] = useState<ServerNotification[]>([])
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
    let seq = 0

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
    es.addEventListener('notification', (e) => {
      try {
        const ev = JSON.parse((e as MessageEvent).data) as Omit<ServerNotification, 'id'>
        // The wire carries no id, and two events can share a timestamp (it is
        // second-resolution), so the id is minted here: a duplicate key would
        // make React reuse the wrong row.
        setNotifications((cur) => {
          const id = `${ev.timestamp}-${seq++}`
          const next = [...cur, { ...ev, id }]
          return next.length > MAX_NOTIFICATIONS ? next.slice(-MAX_NOTIFICATIONS) : next
        })
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

  return { logs, stats, notifications, error, refreshStats }
}
