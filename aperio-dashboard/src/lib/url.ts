// Small helpers for keeping a slice of UI state in the URL query string so the
// active tab and traffic filters survive a reload and can be shared as a link.

export function readParams(): URLSearchParams {
  return new URLSearchParams(window.location.search)
}

/**
 * Writes `params` back to the address bar. `push` adds a history entry (so the
 * browser back button steps through it — used for tab navigation); otherwise it
 * replaces the current entry (used for filter typing, which shouldn't spam
 * history).
 */
export function writeParams(params: URLSearchParams, push = false): void {
  const qs = params.toString()
  const url = `${window.location.pathname}${qs ? `?${qs}` : ''}`
  if (push) window.history.pushState(null, '', url)
  else window.history.replaceState(null, '', url)
}
