import { useCallback, useEffect, useRef, useState } from 'react'

/** Polls `fn` on a fixed interval; returns the latest value and a manual refresh. */
export function usePoll<T>(
  fn: () => Promise<T>,
  intervalMs: number,
): { data: T | null; refresh: () => void } {
  const [data, setData] = useState<T | null>(null)
  const fnRef = useRef(fn)
  fnRef.current = fn

  const refresh = useCallback(() => {
    fnRef
      .current()
      .then(setData)
      .catch(() => {
        // Transient fetch failures keep showing the last good data.
      })
  }, [])

  useEffect(() => {
    refresh()
    const timer = setInterval(refresh, intervalMs)
    return () => clearInterval(timer)
  }, [intervalMs, refresh])

  return { data, refresh }
}
