import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

// Run the hook's effects without a DOM. Deferred replies below control the
// network ordering while the real hook decides which data reaches state.
const fixture = vi.hoisted(() => ({
  states: [] as unknown[],
  effects: [] as Array<() => void | (() => void)>,
  push: undefined as undefined | ((value: unknown) => void),
}))

vi.mock('react', () => ({
  useState(initial: unknown) {
    const index = fixture.states.length
    fixture.states.push(initial)
    return [initial, (value: unknown) => { fixture.states[index] = value }]
  },
  useRef: (current: unknown) => ({ current }),
  useCallback: (callback: unknown) => callback,
  useEffect: (effect: () => void | (() => void)) => { fixture.effects.push(effect) },
}))
vi.mock('@/lib/stream', () => ({
  streamHub: {
    subscribe(_topic: string, push: (value: unknown) => void) {
      fixture.push = push
      return () => { fixture.push = undefined }
    },
    onStatus(callback: (up: boolean) => void) {
      callback(true)
      return () => {}
    },
  },
}))

import { useStream } from './useStream'

function deferred() {
  let resolve!: (value: string[]) => void
  let reject!: (error: Error) => void
  const promise = new Promise<string[]>((yes, no) => { resolve = yes; reject = no })
  return { promise, resolve, reject }
}

let cleanups: Array<void | (() => void)> = []
beforeEach(() => {
  fixture.states = []
  fixture.effects = []
  vi.stubGlobal('document', {
    visibilityState: 'visible', addEventListener() {}, removeEventListener() {},
  })
})
afterEach(() => {
  cleanups.forEach((cleanup) => cleanup?.())
  cleanups = []
  vi.unstubAllGlobals()
})

describe('REST and SSE ordering', () => {
  it.each(['success', 'failure'] as const)('keeps a newer push after an older REST %s', async (result) => {
    const request = deferred()
    useStream('tokens', () => request.promise, 10_000)
    cleanups = fixture.effects.map((effect) => effect())
    fixture.push?.(['new token'])
    if (result === 'success') request.resolve(['old token'])
    else request.reject(new Error('old request failed'))
    await request.promise.catch(() => {})
    expect(fixture.states[0]).toEqual(['new token'])
    expect(fixture.states[1]).toBe(false)
    expect(fixture.states[2]).toBe(false)
  })

  it('allows a REST refresh issued after the push, but ignores the earlier seed', async () => {
    const seed = deferred()
    const refresh = deferred()
    const fetch = vi.fn().mockReturnValueOnce(seed.promise).mockReturnValueOnce(refresh.promise)
    const hook = useStream('tokens', fetch, 10_000)
    cleanups = fixture.effects.map((effect) => effect())
    fixture.push?.(['pushed'])
    hook.refresh()
    refresh.resolve(['refreshed'])
    await refresh.promise
    seed.resolve(['old'])
    await seed.promise
    expect(fixture.states[0]).toEqual(['refreshed'])
  })
})
