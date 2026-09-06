/**
 * One connection for everything the dashboard watches.
 *
 * `/aperio/api/stream` pushes the live view (`stats`, `traffic`,
 * `notification`) and, for every topic named in its query string, the
 * document a page used to poll for (`tokens`, `uptime`, `session`, ...), sent
 * on connect and again whenever it changes. A browser `EventSource` cannot
 * change its subscription once open, so this hub keeps the set of topics the
 * mounted components asked for and reopens the one connection when that set
 * changes, a moment after the last change so a page that mounts five
 * sections opens one stream rather than five.
 *
 * The hub is a plain object rather than a hook so a test can drive it with a
 * fake `EventSource`, and so there is exactly one of it: two hubs would be
 * two connections, which is the thing this exists to avoid.
 */

export type StreamListener = (data: unknown) => void
export type StatusListener = (up: boolean) => void

/** The three events every stream carries; they are never named in the query. */
const BASE_TOPICS = new Set(['stats', 'traffic', 'notification'])

/** How long the hub waits after a subscription change before reopening. */
const REOPEN_DELAY_MS = 30

interface EventSourceLike {
  addEventListener(type: string, listener: (e: MessageEvent) => void): void
  close(): void
  onopen: ((e: Event) => void) | null
  onerror: ((e: Event) => void) | null
}

export type EventSourceFactory = (url: string) => EventSourceLike

export class StreamHub {
  private readonly listeners = new Map<string, Set<StreamListener>>()
  private readonly statusListeners = new Set<StatusListener>()
  private source: EventSourceLike | null = null
  /** The topic set the open connection was opened with. */
  private openedWith = ''
  private reopenTimer: ReturnType<typeof setTimeout> | undefined
  private up = false
  private readonly open: EventSourceFactory

  constructor(open: EventSourceFactory = (url) => new EventSource(url)) {
    this.open = open
  }

  /** The URL the current subscription set asks for. */
  url(): string {
    const extra = [...this.listeners.keys()].filter((t) => !BASE_TOPICS.has(t)).sort()
    return extra.length ? `/aperio/api/stream?topics=${extra.join(',')}` : '/aperio/api/stream'
  }

  /** True while the connection is open; false before the first open and after an error. */
  isUp(): boolean {
    return this.up
  }

  /** Listens for `topic` events; returns the function that stops listening.
   *  The first listener on a topic adds it to the connection, the last one
   *  removes it. */
  subscribe(topic: string, listener: StreamListener): () => void {
    let set = this.listeners.get(topic)
    if (!set) {
      set = new Set()
      this.listeners.set(topic, set)
    }
    set.add(listener)
    this.scheduleReopen()
    return () => {
      const current = this.listeners.get(topic)
      if (!current) return
      current.delete(listener)
      if (current.size === 0) this.listeners.delete(topic)
      this.scheduleReopen()
    }
  }

  /** Runs `listener` with the connection state now and on every change. */
  onStatus(listener: StatusListener): () => void {
    this.statusListeners.add(listener)
    listener(this.up)
    return () => {
      this.statusListeners.delete(listener)
    }
  }

  /** Closes the connection; the next subscription change reopens it. */
  close(): void {
    if (this.reopenTimer) {
      clearTimeout(this.reopenTimer)
      this.reopenTimer = undefined
    }
    this.source?.close()
    this.source = null
    this.openedWith = ''
    this.setUp(false)
  }

  private scheduleReopen(): void {
    if (this.reopenTimer) clearTimeout(this.reopenTimer)
    this.reopenTimer = setTimeout(() => {
      this.reopenTimer = undefined
      this.reconcile()
    }, REOPEN_DELAY_MS)
  }

  /** Opens, reopens or closes the connection so it matches the listeners. */
  private reconcile(): void {
    if (this.listeners.size === 0) {
      if (this.source) this.close()
      return
    }
    const url = this.url()
    if (this.source && this.openedWith === url) return
    this.source?.close()
    this.setUp(false)
    const source = this.open(url)
    this.source = source
    this.openedWith = url
    source.onopen = () => this.setUp(true)
    source.onerror = () => this.setUp(false)
    // Every topic of the connection gets its own named listener; a topic
    // subscribed later reopens the connection through `scheduleReopen`, so
    // the set here is complete for as long as this source lives.
    const topics = new Set([...BASE_TOPICS, ...this.listeners.keys()])
    for (const topic of topics) {
      source.addEventListener(topic, (e) => this.deliver(topic, e))
    }
  }

  private deliver(topic: string, e: MessageEvent): void {
    const set = this.listeners.get(topic)
    if (!set || set.size === 0) return
    let data: unknown
    try {
      data = JSON.parse(e.data as string)
    } catch {
      // A malformed frame is dropped; the next one is whole.
      return
    }
    for (const listener of set) listener(data)
  }

  private setUp(up: boolean): void {
    if (this.up === up) return
    this.up = up
    for (const listener of this.statusListeners) listener(up)
  }
}

/** The dashboard's one hub. Tests build their own with a fake factory. */
export const streamHub = new StreamHub()
