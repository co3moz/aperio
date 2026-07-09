import { useEffect, useState } from 'react'
import { api, type RequestLog } from '../lib/api'

// Keep a bounded live window on the client so a long-lived stream can't grow
// unbounded; the traffic table only renders the newest slice anyway.
const MAX_LOGS = 500
// Fallback polling cadence used only while the SSE stream is unavailable.
const FALLBACK_POLL_MS = 3000

/**
 * Live traffic feed backed by the `/aperio/api/stream` SSE endpoint. Seeds with
 * the recent window via `api.logs()`, then appends each request as it completes.
 * If the stream errors (e.g. a proxy that buffers SSE), it transparently falls
 * back to polling until the stream recovers, so the table never goes stale.
 */
export function useTrafficStream(): { logs: RequestLog[] | null } {
  const [logs, setLogs] = useState<RequestLog[] | null>(null)

  useEffect(() => {
    let cancelled = false
    let pollTimer: ReturnType<typeof setInterval> | undefined

    const reload = () =>
      api
        .logs()
        .then((l) => {
          if (!cancelled) setLogs(l)
        })
        .catch(() => {
          // Leave the last good data on screen; the stream/next poll will retry.
        })

    // Seed immediately so the table isn't empty until the first live event.
    void reload()

    const startFallback = () => {
      if (pollTimer || cancelled) return
      pollTimer = setInterval(() => void reload(), FALLBACK_POLL_MS)
    }
    const stopFallback = () => {
      if (pollTimer) {
        clearInterval(pollTimer)
        pollTimer = undefined
      }
    }

    const es = new EventSource('/aperio/api/stream')
    es.onopen = () => stopFallback()
    es.onmessage = (e) => {
      try {
        const log = JSON.parse(e.data) as RequestLog
        setLogs((cur) => {
          const next = [...(cur ?? []), log]
          return next.length > MAX_LOGS ? next.slice(-MAX_LOGS) : next
        })
      } catch {
        // Ignore malformed frames.
      }
    }
    es.onerror = () => startFallback()

    return () => {
      cancelled = true
      es.close()
      stopFallback()
    }
  }, [])

  return { logs }
}
