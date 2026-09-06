import { useCallback, useEffect, useRef, useState } from 'react'
import { streamHub } from '@/lib/stream'
import type { PollState } from './usePoll'

// Cap the backoff at 2^4 = 16× the base interval while the fallback keeps
// failing, the same rule usePoll follows.
const MAX_BACKOFF_EXP = 4

/**
 * A page's document, pushed over the dashboard's one event stream.
 *
 * `topic` is the stream topic that carries `fetch`'s answer (`tokens` for
 * `api.tokens`); the server sends it on connect and again whenever it
 * changes, so the page updates the moment a colleague edits the store
 * rather than on its next poll. `fetch` is still used: once on mount, so the
 * page is not empty until the connection settles; on `refresh()`, for the
 * caller who just made a change and wants the answer before the push
 * arrives; and every `fallbackMs` while the stream is down, for a proxy that
 * buffers server-sent events. A `null` topic is a plain poll, for the one
 * caller whose question the stream cannot carry (a filtered audit search).
 *
 * The shape is `usePoll`'s, so a section swapping one for the other changes
 * one line. `key` names the question, as it does there: change it and the
 * data clears rather than showing the old answer under the new question.
 */
export function useStream<T>(
  topic: string | null,
  fetch: () => Promise<T>,
  fallbackMs: number,
  key?: string,
): PollState<T> {
  const [data, setData] = useState<T | null>(null)
  const [error, setError] = useState(false)
  const [loading, setLoading] = useState(true)
  const fetchRef = useRef(fetch)
  fetchRef.current = fetch
  const failures = useRef(0)
  const generation = useRef(0)

  useEffect(() => {
    generation.current += 1
    setData(null)
    setError(false)
    setLoading(true)
  }, [key])

  const runOnce = useCallback(async () => {
    const asked = generation.current
    try {
      const value = await fetchRef.current()
      if (asked !== generation.current) return
      setData(value)
      setError(false)
      failures.current = 0
    } catch {
      if (asked !== generation.current) return
      setError(true)
      failures.current = Math.min(failures.current + 1, MAX_BACKOFF_EXP + 1)
    } finally {
      if (asked === generation.current) setLoading(false)
    }
  }, [])

  const refresh = useCallback(() => {
    void runOnce()
  }, [runOnce])

  // The push: whatever the stream sends under the topic is the answer.
  useEffect(() => {
    if (topic === null) return
    const asked = generation.current
    return streamHub.subscribe(topic, (value) => {
      if (asked !== generation.current) return
      setData(value as T)
      setError(false)
      setLoading(false)
      failures.current = 0
    })
  }, [topic, key])

  // The seed, and the fallback poll while the stream is down (or always,
  // for a poll-only question).
  useEffect(() => {
    let cancelled = false
    let timer: ReturnType<typeof setTimeout> | undefined
    let polling = false

    const schedule = () => {
      const factor = 2 ** Math.min(failures.current, MAX_BACKOFF_EXP)
      timer = setTimeout(loop, fallbackMs * factor)
    }
    const loop = async () => {
      if (cancelled || !polling) return
      if (document.visibilityState === 'visible') await runOnce()
      if (!cancelled && polling) schedule()
    }
    const startPolling = () => {
      if (polling) return
      polling = true
      void loop()
    }
    const stopPolling = () => {
      polling = false
      if (timer) clearTimeout(timer)
      timer = undefined
    }

    void runOnce()
    const unwatch =
      topic === null
        ? (startPolling(), undefined)
        : streamHub.onStatus((up) => (up ? stopPolling() : startPolling()))

    const onVisible = () => {
      if (document.visibilityState === 'visible' && polling && !cancelled) {
        if (timer) clearTimeout(timer)
        void loop()
      }
    }
    document.addEventListener('visibilitychange', onVisible)
    return () => {
      cancelled = true
      stopPolling()
      unwatch?.()
      document.removeEventListener('visibilitychange', onVisible)
    }
  }, [topic, fallbackMs, runOnce, key])

  return { data, refresh, error, loading }
}
