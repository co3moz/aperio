import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { StreamHub } from './stream'

/** An EventSource the test drives by hand. */
class FakeSource {
  static opened: FakeSource[] = []
  readonly listeners = new Map<string, ((e: MessageEvent) => void)[]>()
  closed = false
  onopen: ((e: Event) => void) | null = null
  onerror: ((e: Event) => void) | null = null
  readonly url: string
  constructor(url: string) {
    this.url = url
    FakeSource.opened.push(this)
  }
  addEventListener(type: string, listener: (e: MessageEvent) => void) {
    this.listeners.set(type, [...(this.listeners.get(type) ?? []), listener])
  }
  close() {
    this.closed = true
  }
  /** Delivers a frame the way the browser would. */
  emit(type: string, data: unknown) {
    for (const l of this.listeners.get(type) ?? []) {
      l({ data: JSON.stringify(data) } as MessageEvent)
    }
  }
  open() {
    this.onopen?.(new Event('open'))
  }
  fail() {
    this.onerror?.(new Event('error'))
  }
}

function hub() {
  return new StreamHub((url) => new FakeSource(url))
}

describe('StreamHub', () => {
  beforeEach(() => {
    vi.useFakeTimers()
    FakeSource.opened = []
  })
  afterEach(() => {
    vi.useRealTimers()
  })

  it('opens one connection for a burst of subscriptions and names the extra topics', () => {
    const h = hub()
    const seen: unknown[] = []
    h.subscribe('tokens', (d) => seen.push(d))
    h.subscribe('uptime', () => {})
    h.subscribe('stats', () => {})
    expect(FakeSource.opened).toHaveLength(0)
    vi.advanceTimersByTime(50)
    expect(FakeSource.opened).toHaveLength(1)
    // The base three are never named; the rest are, sorted, so the same set
    // is the same URL and never a needless reopen.
    expect(FakeSource.opened[0].url).toBe('/aperio/api/stream?topics=tokens,uptime')
    FakeSource.opened[0].emit('tokens', [{ name: 'ci' }])
    expect(seen).toEqual([[{ name: 'ci' }]])
  })

  it('reopens when the topic set changes and closes when nobody listens', () => {
    const h = hub()
    const off = h.subscribe('tokens', () => {})
    vi.advanceTimersByTime(50)
    const first = FakeSource.opened[0]
    // The same set again: nothing happens.
    const off2 = h.subscribe('tokens', () => {})
    vi.advanceTimersByTime(50)
    expect(FakeSource.opened).toHaveLength(1)
    expect(first.closed).toBe(false)
    // A new topic: the old connection is replaced by one that asks for both.
    h.subscribe('users', () => {})
    vi.advanceTimersByTime(50)
    expect(first.closed).toBe(true)
    expect(FakeSource.opened[1].url).toBe('/aperio/api/stream?topics=tokens,users')
    // Down to the base stream when only a base topic is left.
    const offStats = h.subscribe('stats', () => {})
    off()
    off2()
    vi.advanceTimersByTime(50)
    expect(FakeSource.opened[2].url).toBe('/aperio/api/stream?topics=users')
    offStats()
    vi.advanceTimersByTime(50)
    expect(FakeSource.opened).toHaveLength(3)
  })

  it('reports the connection state to whoever asks, now and on change', () => {
    const h = hub()
    const states: boolean[] = []
    h.onStatus((up) => states.push(up))
    expect(states).toEqual([false])
    h.subscribe('stats', () => {})
    vi.advanceTimersByTime(50)
    const source = FakeSource.opened[0]
    source.open()
    source.fail()
    source.open()
    expect(states).toEqual([false, true, false, true])
    expect(h.isUp()).toBe(true)
  })

  it('drops a malformed frame and delivers the next one', () => {
    const h = hub()
    const seen: unknown[] = []
    h.subscribe('maintenance', (d) => seen.push(d))
    vi.advanceTimersByTime(50)
    const source = FakeSource.opened[0]
    for (const l of source.listeners.get('maintenance') ?? []) {
      l({ data: '{not json' } as MessageEvent)
    }
    source.emit('maintenance', [])
    expect(seen).toEqual([[]])
  })
})
