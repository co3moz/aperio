import { createContext, useContext, useEffect } from 'react'

/**
 * Where a form reports that it holds edits nobody has saved.
 *
 * Only the thing wrapping the form knows what "leaving" means, for the
 * settings dialog it is closing it or switching panes, so the form states the
 * fact and the wrapper decides what to do about it.
 */
export const UnsavedContext = createContext<(dirty: boolean) => void>(() => {})

/**
 * Declares that this form currently has unsaved edits.
 *
 * Two exits have to be covered and they need different mechanisms. Leaving
 * within the app is ours to intercept, which is what the context is for. A
 * reload is not: the browser's own confirmation is the only thing that can
 * stop an F5, and it is only armed while a `beforeunload` listener is
 * attached, so the listener goes up exactly for as long as there is something
 * to lose.
 */
export function useUnsavedChanges(dirty: boolean) {
  const report = useContext(UnsavedContext)

  useEffect(() => {
    report(dirty)
    // Unmounting is not saving, but it does mean these edits are already gone
    // and can no longer block anything.
    return () => report(false)
  }, [dirty, report])

  useEffect(() => {
    if (!dirty) return
    const onBeforeUnload = (e: BeforeUnloadEvent) => e.preventDefault()
    window.addEventListener('beforeunload', onBeforeUnload)
    return () => window.removeEventListener('beforeunload', onBeforeUnload)
  }, [dirty])
}
