import { describe, expect, it } from 'vitest'
import { CLIENT_GROUPS, SERVER_GROUPS, essentialRank, isEssential, type GroupSpec } from './configGroups'

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

describe('advanced groups', () => {
  // The "Advanced features" line is drawn once, before the first advanced
  // group; a core group after it would be filed under the wrong heading.
  const lastCoreBeforeFirstAdvanced = (groups: GroupSpec[]) => {
    const first = groups.findIndex((g) => g.advanced)
    return first < 0 || groups.slice(first).every((g) => g.advanced)
  }

  it('come after every core group, on both sides', () => {
    expect(lastCoreBeforeFirstAdvanced(CLIENT_GROUPS)).toBe(true)
    expect(lastCoreBeforeFirstAdvanced(SERVER_GROUPS)).toBe(true)
  })

  it('leave the core groups first, and claim each key once', () => {
    for (const groups of [CLIENT_GROUPS, SERVER_GROUPS]) {
      expect(groups[0].advanced).toBeFalsy()
      const keys = groups.flatMap((g) => g.keys)
      expect(new Set(keys).size).toBe(keys.length)
    }
  })
})
