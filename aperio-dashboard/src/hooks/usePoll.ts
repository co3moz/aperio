import { useCallback, useEffect, useRef, useState } from 'react'

export interface PollState<T> {
  data: T | null
  refresh: () => void
  /** True after the most recent poll failed (the last good `data` is kept). */
  error: boolean
  /** True until the first poll settles (success or failure). */
  loading: boolean
}

// Cap the backoff at 2^4 = 16× the base interval so a long outage doesn't stop
// the dashboard from ever retrying.
const MAX_BACKOFF_EXP = 4

/** Polls `fn` on an interval; returns the latest value, a manual refresh, and
 *  the connection state. Polling pauses while the tab is hidden (resuming on
 *  focus) and backs off exponentially while the endpoint keeps failing. */
export function usePoll<T>(fn: () => Promise<T>, intervalMs: number): PollState<T> {
  const [data, setData] = useState<T | null>(null)
  const [error, setError] = useState(false)
  const [loading, setLoading] = useState(true)
  const fnRef = useRef(fn)
  fnRef.current = fn
  const failures = useRef(0)

  const runOnce = useCallback(async () => {
    try {
      const value = await fnRef.current()
      setData(value)
      setError(false)
      failures.current = 0
    } catch {
      // Keep the last good data on screen but flag the failure so callers can
      // surface that what's shown may be stale.
      setError(true)
      failures.current = Math.min(failures.current + 1, MAX_BACKOFF_EXP + 1)
    } finally {
      setLoading(false)
    }
  }, [])

  const refresh = useCallback(() => {
    void runOnce()
  }, [runOnce])

  useEffect(() => {
    let cancelled = false
    let timer: ReturnType<typeof setTimeout>

    const schedule = () => {
      const factor = 2 ** Math.min(failures.current, MAX_BACKOFF_EXP)
      timer = setTimeout(loop, intervalMs * factor)
    }
    const loop = async () => {
      if (cancelled) return
      if (document.visibilityState === 'visible') await runOnce()
      if (!cancelled) schedule()
    }

    void loop()

    const onVisible = () => {
      if (document.visibilityState === 'visible' && !cancelled) {
        clearTimeout(timer)
        void loop()
      }
    }
    document.addEventListener('visibilitychange', onVisible)
    return () => {
      cancelled = true
      clearTimeout(timer)
      document.removeEventListener('visibilitychange', onVisible)
    }
  }, [intervalMs, runOnce])

  return { data, refresh, error, loading }
}
