import { cn } from '@/lib/utils'

/**
 * The Aperio wordmark: the product name set beside the mark.
 *
 * `lang="en"` is not decoration. The dashboard sets `<html lang>` to the UI
 * language, and `text-transform: uppercase` is locale-aware: under `lang="tr"`
 * the `i` in "Aperio" uppercases to a dotted `İ`, so the brand would read
 * APERİO to every Turkish user. Pinning the fragment to English keeps the
 * casing rules English, and tells a screen reader to pronounce it as an
 * English word rather than through the surrounding language's phonology.
 *
 * The text stays "Aperio" and is uppercased in CSS rather than written in
 * capitals, so assistive technology still receives a word instead of
 * something it may decide to spell out letter by letter.
 */
export function AperioWordmark({ className }: { className?: string }) {
  return (
    <span lang="en" className={cn('font-wordmark uppercase tracking-tight', className)}>
      Aperio
    </span>
  )
}
