/**
 * Whether a page's "about" paragraph is folded away (planned_features #161).
 *
 * A paragraph that explains a page is useful the first time and noise the
 * fiftieth, so it opens on the first visit and stays closed once the reader
 * closed it. The memory is per viewer and per page, which is what browser
 * storage is for; it comes back empty in a private window, and then the
 * paragraph simply shows again.
 */

const PREFIX = 'aperio-about-closed:'

/** The paragraphs long enough to fold; a sentence stays where it is. */
export const ABOUT_FOLD_CHARS = 140

interface StorageLike {
  getItem(key: string): string | null
  setItem(key: string, value: string): void
  removeItem(key: string): void
}

function storage(): StorageLike | null {
  try {
    return typeof localStorage === 'undefined' ? null : localStorage
  } catch {
    return null
  }
}

/** True when the reader closed this page's paragraph before. */
export function aboutClosed(page: string, store: StorageLike | null = storage()): boolean {
  try {
    return store?.getItem(PREFIX + page) === '1'
  } catch {
    return false
  }
}

/** Remembers the reader's choice; a failure to write is a paragraph that
 *  shows again next time, which is the harmless direction. */
export function rememberAbout(page: string, closed: boolean, store: StorageLike | null = storage()): void {
  try {
    if (closed) store?.setItem(PREFIX + page, '1')
    else store?.removeItem(PREFIX + page)
  } catch {
    // Storage full or blocked: nothing to do, the paragraph shows again.
  }
}
