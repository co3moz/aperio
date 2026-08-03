import type { TFn } from '@/i18n'
import type { RequestTimeline } from '@/lib/api'

/** One row of the inspector's waterfall: a labelled span of the request. */
export interface Stage {
  label: string
  from: number
  to: number
  /** Anchored by splitting the unaccounted transit, not measured directly. */
  estimated?: boolean
}

/**
 * The rows of one request's waterfall.
 *
 * Pure, and in `lib` rather than inside the dialog, so the one decision it
 * makes can be tested: whether this capture carries the client's own stage
 * timings, or only the coarse round trip.
 *
 * That decision has exactly one trap, and it is the bug this function was
 * extracted for. The optional stages arrive as the JSON value `null` with the
 * key present, because the server serializes `Option<u64>` without
 * `skip_serializing_if`. A check written as `!== undefined` is true for every
 * one of them, so a streamed response, whose client stages are never
 * captured, took the detailed branch and drew six rows reading `+null µs`.
 * `!= null` is the comparison that matches the wire.
 */
export function timelineStages(tl: RequestTimeline, t: TFn): Stage[] {
  const stages: Stage[] = [{ label: t('queued & routed'), from: 0, to: tl.dispatched_us }]
  if (
    tl.client_received_us != null &&
    tl.backend_sent_us != null &&
    tl.backend_first_byte_us != null &&
    tl.backend_done_us != null &&
    tl.client_responded_us != null
  ) {
    stages.push(
      {
        label: t('tunnel → client'),
        from: tl.dispatched_us,
        to: tl.client_received_us,
        estimated: true,
      },
      {
        label: t('client processing'),
        from: tl.client_received_us,
        to: tl.backend_sent_us,
        estimated: true,
      },
      {
        label: t('backend wait (first byte)'),
        from: tl.backend_sent_us,
        to: tl.backend_first_byte_us,
        estimated: true,
      },
      {
        label: t('backend body'),
        from: tl.backend_first_byte_us,
        to: tl.backend_done_us,
        estimated: true,
      },
      {
        label: t('client → tunnel'),
        from: tl.backend_done_us,
        to: tl.client_responded_us,
        estimated: true,
      },
      {
        label: t('tunnel → server'),
        from: tl.client_responded_us,
        to: tl.response_received_us,
        estimated: true,
      },
    )
  } else {
    // What a streamed response, or a client too old to report its stages,
    // was always meant to get: one honest row instead of six invented ones.
    stages.push({
      label: t('tunnel round-trip (client & backend)'),
      from: tl.dispatched_us,
      to: tl.response_received_us,
    })
  }
  stages.push({ label: t('server → visitor'), from: tl.response_received_us, to: tl.finished_us })
  return stages
}
