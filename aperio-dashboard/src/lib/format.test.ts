import { describe, expect, it } from 'vitest'
import {
  formatBandwidth,
  formatBytes,
  formatExpiry,
  formatUptime,
  parseByteSize,
  splitList,
} from './format'

describe('formatBytes', () => {
  it('formats byte magnitudes', () => {
    expect(formatBytes(0)).toBe('0 B')
    expect(formatBytes(512)).toBe('512 B')
    expect(formatBytes(1024)).toBe('1 KB')
    expect(formatBytes(1536)).toBe('1.5 KB')
    expect(formatBytes(1048576)).toBe('1 MB')
  })
})

describe('parseByteSize', () => {
  it('parses human sizes to bytes (binary units)', () => {
    expect(parseByteSize('1024')).toBe(1024)
    expect(parseByteSize('1kb')).toBe(1024)
    expect(parseByteSize('1.5 GB')).toBe(Math.round(1.5 * 1024 ** 3))
    expect(parseByteSize('512K')).toBe(512 * 1024)
    // Comma decimal separator is accepted.
    expect(parseByteSize('2,5mb')).toBe(Math.round(2.5 * 1024 ** 2))
  })
  it('rejects garbage', () => {
    expect(parseByteSize('')).toBeNull()
    expect(parseByteSize('abc')).toBeNull()
    expect(parseByteSize('10 petabytes')).toBeNull()
  })
})

describe('formatBandwidth', () => {
  it('renders bytes/second as a bit rate', () => {
    expect(formatBandwidth(1_000_000)).toBe('8 Mbit/s')
    expect(formatBandwidth(125)).toBe('1 kbit/s')
    expect(formatBandwidth(0)).toBe('0 bit/s')
  })
})

describe('formatUptime', () => {
  it('drops empty leading units', () => {
    expect(formatUptime(0)).toBe('0s')
    expect(formatUptime(59)).toBe('59s')
    expect(formatUptime(61)).toBe('1m 1s')
    expect(formatUptime(3661)).toBe('1h 1m 1s')
    // A zero minute between hours and seconds is omitted.
    expect(formatUptime(3601)).toBe('1h 1s')
  })
})

describe('formatExpiry', () => {
  it('handles never / expired', () => {
    expect(formatExpiry(null, false)).toBe('never')
    const ts = 1_700_000_000
    expect(formatExpiry(ts, true)).toContain('expired')
    expect(formatExpiry(ts, false)).not.toContain('expired')
  })
})

describe('splitList', () => {
  it('splits, trims and drops empties', () => {
    expect(splitList('a, b ,,c')).toEqual(['a', 'b', 'c'])
    expect(splitList('   ')).toEqual([])
    expect(splitList('single')).toEqual(['single'])
  })
})
