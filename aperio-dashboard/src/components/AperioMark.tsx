import { LOGO_PATHS, LOGO_VIEWBOX } from '@/lib/logo'
import { cn } from '@/lib/utils'

/**
 * The Aperio mark.
 *
 * Filled with `currentColor` rather than a fixed brand colour, so it takes
 * the colour of whatever it sits in and needs no light/dark variant of its
 * own: on the sidebar tile it is the tile's foreground, on the login card it
 * is the card's. One asset, correct in both themes, and correct again if the
 * palette ever changes.
 */
export function AperioMark({ className }: { className?: string }) {
  return (
    <svg
      viewBox={LOGO_VIEWBOX}
      xmlns="http://www.w3.org/2000/svg"
      fill="currentColor"
      role="img"
      aria-label="Aperio"
      className={cn('size-4', className)}
    >
      {LOGO_PATHS.map((d) => (
        <path key={d.slice(0, 24)} d={d} />
      ))}
    </svg>
  )
}
