import { describe, expect, it } from 'vitest'
import { detailOf, isUrgent, severityOf } from './notifications'

describe('severityOf', () => {
  it('mirrors the webhook card colours', () => {
    expect(severityOf('client_connected')).toBe('good')
    expect(severityOf('client_disconnected')).toBe('bad')
    expect(severityOf('token_expiring')).toBe('warn')
  })

  it('treats an unknown event as information, not an alarm', () => {
    // A newer server can emit an event this dashboard has never heard of.
    expect(severityOf('something_new')).toBe('info')
    expect(isUrgent('something_new')).toBe(false)
  })
})

describe('detailOf', () => {
  it('puts the identifying field first', () => {
    expect(detailOf({ ip: '10.0.0.1', name: 'api' })).toBe('name: api · ip: 10.0.0.1')
  })

  it('skips nested values and empty fields', () => {
    expect(detailOf({ name: 'api', meta: { a: 1 }, reason: null })).toBe('name: api')
  })

  it('stays one line', () => {
    const many = { a: 1, b: 2, c: 3, d: 4, e: 5 }
    expect(detailOf(many).split(' · ')).toHaveLength(3)
  })

  it('survives an event with no fields', () => {
    expect(detailOf({})).toBe('')
  })
})
