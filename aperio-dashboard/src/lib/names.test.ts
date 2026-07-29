import { describe, expect, it } from 'vitest'
import { nameError, slug } from './names'

// The rule is enforced by the server; this is the copy the forms check
// against, so the two are pinned to the same examples the Rust tests use.
describe('handles', () => {
  it('accepts an identifier and refuses everything that could be written twice', () => {
    expect(nameError('organization', 'pg_main')).toBeNull()
    expect(nameError('organization', 'payments2')).toBeNull()
    expect(nameError('organization', '')).toContain('empty')
    expect(nameError('organization', 'Acme')).toContain('acme')
    expect(nameError('organization', 'pg-main')).toContain('pg_main')
    expect(nameError('organization', 'db.primary')).toContain('db_primary')
    expect(nameError('organization', 'a'.repeat(65))).toContain('longer')
  })

  it('proposes a handle from a display name', () => {
    expect(slug('Acme Inc.')).toBe('acme_inc')
    expect(slug('Müşteri Portalı')).toBe('musteri_portali')
    expect(slug('Ödeme Servisi')).toBe('odeme_servisi')
    expect(slug('Größe')).toBe('grosse')
    // Nothing readable left: still a handle, since something has to be one.
    expect(slug('数据库')).toBe('unnamed')
    expect(nameError('test', slug('数据库'))).toBeNull()
  })
})
