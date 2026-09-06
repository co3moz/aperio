import { describe, expect, it } from 'vitest'
import { aboutClosed, rememberAbout } from './about'

function fakeStore() {
  const map = new Map<string, string>()
  return {
    getItem: (k: string) => map.get(k) ?? null,
    setItem: (k: string, v: string) => void map.set(k, v),
    removeItem: (k: string) => void map.delete(k),
    map,
  }
}

describe('about paragraphs', () => {
  it('open on the first visit and stay closed once closed, per page', () => {
    const store = fakeStore()
    expect(aboutClosed('Topology', store)).toBe(false)
    rememberAbout('Topology', true, store)
    expect(aboutClosed('Topology', store)).toBe(true)
    expect(aboutClosed('Autoscaling', store)).toBe(false)
    rememberAbout('Topology', false, store)
    expect(aboutClosed('Topology', store)).toBe(false)
    expect(store.map.size).toBe(0)
  })

  it('treat missing or broken storage as a first visit', () => {
    expect(aboutClosed('Topology', null)).toBe(false)
    const broken = {
      getItem: () => {
        throw new Error('blocked')
      },
      setItem: () => {
        throw new Error('blocked')
      },
      removeItem: () => {},
    }
    expect(aboutClosed('Topology', broken)).toBe(false)
    expect(() => rememberAbout('Topology', true, broken)).not.toThrow()
  })
})
