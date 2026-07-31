import { useEffect } from 'react'

/** Things one section does that another section is showing.
 *
 *  Sections poll independently, which is right for data that drifts on its
 *  own but wrong for a change the user just made: creating an organization
 *  left the sidebar's picker listing the old set until its own 30s poll came
 *  round, and the picker is where you go next. A named signal costs nothing
 *  and keeps the two ends decoupled. */
export type AppEvent = 'orgs-changed'

const name = (event: AppEvent) => `aperio:${event}`

/** Announces a change every listening section should pick up now. */
export function emitAppEvent(event: AppEvent) {
  window.dispatchEvent(new Event(name(event)))
}

/** Runs `handler` whenever `event` is emitted. Typically a poll's `refresh`. */
export function useAppEvent(event: AppEvent, handler: () => void) {
  useEffect(() => {
    const listener = () => handler()
    window.addEventListener(name(event), listener)
    return () => window.removeEventListener(name(event), listener)
  }, [event, handler])
}
