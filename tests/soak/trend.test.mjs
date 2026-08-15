// What the trend rule says about series whose answer is known.
//
//   node --test 'tests/soak/*.test.mjs'
//
// These exist because the rule is the whole gate and the gate is otherwise
// only exercised by a weekly run that takes minutes and needs load. A rule
// that fires on a sawtooth would fail the build every week; one that sleeps
// through a slow leak would let the README's claim rot. Both are cheap to
// check here and expensive to discover there.
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { judge, median, slope } from './trend.mjs'

const MB = 1024 * 1024
const EVERY = 5_000

/** A series of `n` samples, five seconds apart, from `f(i)`. */
function series(n, f) {
  return Array.from({ length: n }, (_, i) => ({ atMs: i * EVERY, rssBytes: f(i) }))
}

test('slope and median are what they say', () => {
  assert.equal(slope([{ x: 0, y: 0 }, { x: 10, y: 100 }]), 10)
  assert.equal(slope([{ x: 0, y: 5 }, { x: 10, y: 5 }]), 0)
  assert.equal(slope([]), 0)
  assert.equal(median([3, 1, 2]), 2)
  assert.equal(median([4, 1, 3, 2]), 2.5)
})

test('a flat curve is flat, even with the jitter a real sample has', () => {
  const wobble = [0, 0.4, -0.3, 0.2, -0.5, 0.1, 0.3, -0.2, 0.5, -0.4, 0.2, 0]
  const verdict = judge(series(12, (i) => 14 * MB + wobble[i] * MB))
  assert.equal(verdict.verdict, 'flat', JSON.stringify(verdict))
})

test('a steady leak is caught', () => {
  // 1 MB per sample on a 14 MB baseline: what a per-request allocation that
  // is never freed looks like.
  const verdict = judge(series(12, (i) => 14 * MB + i * MB))
  assert.equal(verdict.verdict, 'growing', JSON.stringify(verdict))
  assert.ok(verdict.trending && verdict.stepped)
  assert.ok(verdict.slopeBytesPerMinute > 0)
})

test('a slow leak is caught too, which is the one a threshold would miss', () => {
  // 2% of the baseline per sample. No absolute limit would notice this inside
  // one run, and over a week of running it is the whole process.
  const verdict = judge(series(20, (i) => 14 * MB * (1 + i * 0.02)))
  assert.equal(verdict.verdict, 'growing', JSON.stringify(verdict))
})

test('a sawtooth is not a leak', () => {
  // Rises and is reclaimed, repeatedly, ending where it started. A rule that
  // looked only at first-versus-last, or only at a peak, would call this a
  // leak every week.
  const verdict = judge(series(16, (i) => 14 * MB + (i % 4) * 3 * MB))
  assert.equal(verdict.verdict, 'flat', JSON.stringify(verdict))
})

test('a cache that fills once and settles is not a leak', () => {
  // The shape of a lazily-built cache: climbs early, then holds. The step
  // between quarters is real, so this is exactly the case that needs *both*
  // conditions, and the reason a first-versus-last comparison alone would be
  // the wrong rule.
  const verdict = judge(series(24, (i) => 14 * MB + Math.min(i, 5) * 0.4 * MB))
  assert.equal(verdict.verdict, 'flat', JSON.stringify(verdict))
})

test('one high outlier does not make a trend', () => {
  const verdict = judge(series(16, (i) => (i === 9 ? 40 * MB : 14 * MB)))
  assert.equal(verdict.verdict, 'flat', JSON.stringify(verdict))
})

test('too few samples is inconclusive, not a pass', () => {
  // The failure worth avoiding: a run that died early reporting "flat" and
  // being read as evidence.
  const verdict = judge(series(3, () => 14 * MB))
  assert.equal(verdict.verdict, 'inconclusive')
  assert.match(verdict.reason, /samples/)
})

test('the tolerance is what decides a borderline curve', () => {
  const borderline = series(16, (i) => 14 * MB + i * 0.15 * MB)
  assert.equal(judge(borderline, { growthTolerance: 0.5 }).verdict, 'flat')
  assert.equal(judge(borderline, { growthTolerance: 0.02 }).verdict, 'growing')
})
