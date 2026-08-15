// Deciding whether a memory curve grows (`planned_features.md` #98).
//
// Separate from the runner because this is the part that decides pass or
// fail, and it is the part that can be checked without generating any load:
// hand it a series and it says what it thinks of it. `trend.test.mjs` does
// exactly that, against series whose answer is known.
//
// **The rule is a trend, not a threshold**, which is the entry's requirement
// and the only rule that means anything here. An absolute limit ("under
// 200 MB") passes a leak that has not run long enough and fails a build that
// legitimately caches more; what the README claims is that memory does not
// grow with request count, and growth is a shape rather than a number.
//
// Two things have to be true before a run is called growing, and requiring
// both is what keeps it from firing on noise:
//
//  1. The least-squares slope over the plateau, extrapolated across it,
//     accounts for more than `growthTolerance` of the starting RSS.
//  2. The median of the last quarter is higher than the median of the first
//     quarter by more than the same fraction.
//
// A leak satisfies both. A garbage collector's sawtooth, a lazily-built cache
// that settles, and a noisy sample satisfy at most one.

/** Least-squares slope of `y` against `x`, in y-units per x-unit. */
export function slope(points) {
  const n = points.length
  if (n < 2) return 0
  const meanX = points.reduce((a, p) => a + p.x, 0) / n
  const meanY = points.reduce((a, p) => a + p.y, 0) / n
  let num = 0
  let den = 0
  for (const p of points) {
    num += (p.x - meanX) * (p.y - meanY)
    den += (p.x - meanX) ** 2
  }
  return den === 0 ? 0 : num / den
}

export function median(values) {
  if (values.length === 0) return 0
  const sorted = [...values].sort((a, b) => a - b)
  const mid = Math.floor(sorted.length / 2)
  return sorted.length % 2 ? sorted[mid] : (sorted[mid - 1] + sorted[mid]) / 2
}

/**
 * Judges one process's samples.
 *
 * `samples` are `{ atMs, rssBytes }`, already narrowed to the plateau: the
 * ramp is deliberately not this function's problem, because memory is
 * *supposed* to rise while load is being added and a rule that looked at the
 * ramp would be measuring the ramp.
 */
export function judge(samples, { growthTolerance = 0.1, minSamples = 6 } = {}) {
  if (samples.length < minSamples) {
    return {
      verdict: 'inconclusive',
      reason: `only ${samples.length} samples over the plateau, needs ${minSamples}`,
    }
  }

  const points = samples.map((s) => ({ x: s.atMs, y: s.rssBytes }))
  const first = samples[0].rssBytes
  const spanMs = samples[samples.length - 1].atMs - samples[0].atMs
  const perMs = slope(points)
  const projected = perMs * spanMs

  const quarter = Math.max(1, Math.floor(samples.length / 4))
  const startMedian = median(samples.slice(0, quarter).map((s) => s.rssBytes))
  const endMedian = median(samples.slice(-quarter).map((s) => s.rssBytes))
  const step = endMedian - startMedian

  const allowed = first * growthTolerance
  const trending = projected > allowed
  const stepped = step > allowed

  return {
    verdict: trending && stepped ? 'growing' : 'flat',
    // Everything the verdict was made of, because a report that says only
    // "growing" sends the reader back to the raw samples anyway.
    firstRssBytes: first,
    lastRssBytes: samples[samples.length - 1].rssBytes,
    spanMs,
    slopeBytesPerMinute: Math.round(perMs * 60_000),
    projectedGrowthBytes: Math.round(projected),
    startMedianBytes: Math.round(startMedian),
    endMedianBytes: Math.round(endMedian),
    quarterStepBytes: Math.round(step),
    allowedBytes: Math.round(allowed),
    trending,
    stepped,
  }
}

/** Human-sized bytes, for the report. */
export function mb(bytes) {
  return `${(bytes / 1024 / 1024).toFixed(1)} MB`
}
