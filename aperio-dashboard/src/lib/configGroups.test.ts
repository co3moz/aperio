import { describe, expect, it } from 'vitest'
import { essentialRank, isEssential } from './configGroups'

describe('essential keys', () => {
  it('puts the things you decide first at the front', () => {
    // Schema order is alphabetical, which is useless: a service entry has to
    // say what it is and where it answers before how long it may take.
    const entry = ['timeout', 'target', 'max_concurrent', 'name', 'hostname']
    const sorted = [...entry].sort((a, b) => essentialRank(a) - essentialRank(b))
    expect(sorted.slice(0, 3)).toEqual(['name', 'target', 'hostname'])
  })

  it('keeps unranked keys out of the essential tier', () => {
    expect(isEssential('target')).toBe(true)
    expect(isEssential('name')).toBe(true)
    expect(isEssential('max_message_size')).toBe(false)
    expect(essentialRank('max_message_size')).toBe(Number.MAX_SAFE_INTEGER)
  })

  it('ranks a block’s own switch above the settings it gates', () => {
    // `enabled: false` makes the rest of a block inert, so it is decided first.
    expect(essentialRank('enabled')).toBeLessThan(essentialRank('max_bytes'))
  })
})
