import { describe, expect, it } from 'vitest'
import type { RequestTimeline } from '@/lib/api'
import { timelineStages } from './timeline'

/** The identity translator: these tests are about the shape, not the wording. */
const t = (key: string) => key

const base: RequestTimeline = {
  client_ready_us: null,
  admitted_us: null,
  selected_us: null,
  dispatched_us: 110,
  client_received_us: null,
  backend_sent_us: null,
  backend_first_byte_us: null,
  backend_done_us: null,
  client_responded_us: null,
  response_received_us: 3_260,
  finished_us: 3_430,
  estimated_anchor: false,
}

describe('timelineStages', () => {
  it('collapses to one round-trip row when the client stages are absent', () => {
    // A streamed response never carries them. The bug was that they arrive as
    // `null` rather than missing, so a `!== undefined` check took the detailed
    // branch and drew six rows of "+null µs".
    const stages = timelineStages(base, t)
    expect(stages.map((s) => s.label)).toEqual([
      'queued & routed',
      'tunnel round-trip (client & backend)',
      'server → visitor',
    ])
    // Nothing rendered may be non-finite: that is what put "null" on screen.
    for (const s of stages) {
      expect(Number.isFinite(s.from)).toBe(true)
      expect(Number.isFinite(s.to)).toBe(true)
    }
  })

  it('expands to the client stages when every one of them is present', () => {
    const stages = timelineStages(
      {
        ...base,
        client_received_us: 200,
        backend_sent_us: 300,
        backend_first_byte_us: 900,
        backend_done_us: 1_500,
        client_responded_us: 1_600,
      },
      t,
    )
    expect(stages).toHaveLength(8)
    expect(stages[1].label).toBe('tunnel → client')
    expect(stages[1].estimated).toBe(true)
  })

  it('takes the coarse row when a stage other than the body end is missing', () => {
    // Partial timings would draw a waterfall with a hole in it, which reads as
    // a measurement rather than as an absence.
    const stages = timelineStages(
      {
        ...base,
        client_received_us: 200,
        backend_sent_us: 300,
        backend_first_byte_us: 900,
        backend_done_us: 1_500,
        client_responded_us: null,
      },
      t,
    )
    expect(stages).toHaveLength(3)
  })

  it('merges the last two rows when only the body end is missing', () => {
    // The head of a streamed response: every stage up to the head is
    // measured, and the body has not finished, so there is no end to draw.
    const stages = timelineStages(
      {
        ...base,
        client_received_us: 200,
        backend_sent_us: 300,
        backend_first_byte_us: 900,
        client_responded_us: 1_600,
      },
      t,
    )
    expect(stages.map((s) => s.label)).toEqual([
      'queued & routed',
      'tunnel → client',
      'client processing',
      'backend wait (first byte)',
      'response head → tunnel (body still streaming)',
      'tunnel → server',
      'server → visitor',
    ])
    expect(stages[4].from).toBe(900)
    expect(stages[4].to).toBe(1_600)
  })

  it('covers the whole request without a gap', () => {
    for (const tl of [
      base,
      {
        ...base,
        client_received_us: 200,
        backend_sent_us: 300,
        backend_first_byte_us: 900,
        backend_done_us: 1_500,
        client_responded_us: 1_600,
      },
    ]) {
      const stages = timelineStages(tl, t)
      expect(stages[0].from).toBe(0)
      expect(stages[stages.length - 1].to).toBe(tl.finished_us)
      // Each row starts where the previous one ended: a waterfall with a gap
      // is a waterfall that has lost time somewhere without saying so.
      for (let i = 1; i < stages.length; i++) {
        expect(stages[i].from).toBe(stages[i - 1].to)
      }
    }
  })

  it('tells the pre-dispatch stages apart when the server recorded them', () => {
    // These are the server's own marks, so a streamed capture still carries
    // them. The dashboard used to drop all three and show one "queued &
    // routed" row, while scripts/profile-request.sh printed the four the
    // server had actually measured.
    const stages = timelineStages(
      { ...base, client_ready_us: 15, admitted_us: 16, selected_us: 26 },
      t,
    )
    expect(stages.map((s) => s.label)).toEqual([
      'waiting for a client',
      'admission (concurrency)',
      'routing',
      'dispatch prep',
      'tunnel round-trip (client & backend)',
      'server → visitor',
    ])
    expect(stages[0].to).toBe(15)
    expect(stages[3].to).toBe(110)
  })

  it('keeps the one coarse row when the pre-dispatch marks are partial', () => {
    const stages = timelineStages({ ...base, client_ready_us: 15 }, t)
    expect(stages[0].label).toBe('queued & routed')
    expect(stages[0].to).toBe(110)
  })
})
