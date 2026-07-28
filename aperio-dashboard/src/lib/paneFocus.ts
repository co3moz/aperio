import { createContext, useContext } from 'react'

/**
 * What a pane should reveal the moment it opens.
 *
 * The command palette can name something finer than a pane — one setting out
 * of sixty, or the form for adding a user — and the pane is the only thing
 * that knows how to get there. So the palette states the target and the pane
 * acts on it.
 *
 * `seq` is what makes asking twice work: picking the same setting again is a
 * new request even though the target string has not changed, and an effect
 * keyed only on the string would ignore it.
 */
export interface PaneFocus {
  target: string
  seq: number
}

export const PaneFocusContext = createContext<PaneFocus | null>(null)

export function usePaneFocus(): PaneFocus | null {
  return useContext(PaneFocusContext)
}
