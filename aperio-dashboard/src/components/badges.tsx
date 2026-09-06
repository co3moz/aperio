import { Badge } from '@/components/ui/badge'
import { cn } from '@/lib/utils'

// Tint recipes reused across tables for consistent semantic colors.
export const TINT = {
  green: 'border-transparent bg-emerald-500/15 text-emerald-700 dark:text-emerald-400',
  red: 'border-transparent bg-red-500/15 text-red-700 dark:text-red-400',
  amber: 'border-transparent bg-amber-500/15 text-amber-700 dark:text-amber-400',
  blue: 'border-transparent bg-sky-500/15 text-sky-700 dark:text-sky-400',
  lime: 'border-transparent bg-lime-500/15 text-lime-700 dark:text-lime-400',
  gray: 'border-transparent bg-muted text-muted-foreground',
} as const

export type Tint = keyof typeof TINT

export function TintBadge({
  tint,
  className,
  children,
}: {
  tint: Tint
  className?: string
  children: React.ReactNode
}) {
  return <Badge className={cn(TINT[tint], className)}>{children}</Badge>
}

const METHOD_TINTS: Record<string, Tint> = {
  GET: 'blue',
  POST: 'green',
  PUT: 'amber',
  PATCH: 'amber',
  DELETE: 'red',
}

export function MethodBadge({ method }: { method: string }) {
  const m = method.toUpperCase()
  return (
    <TintBadge tint={METHOD_TINTS[m] ?? 'gray'} className="font-mono">
      {m}
    </TintBadge>
  )
}

export function StatusBadge({ status, error }: { status: number | null; error?: string | null }) {
  if (error) return <TintBadge tint="red">ERR</TintBadge>
  if (!status) return <TintBadge tint="gray">-</TintBadge>
  const tint: Tint = status < 300 ? 'green' : status < 400 ? 'blue' : status < 500 ? 'amber' : 'red'
  return (
    <TintBadge tint={tint} className="font-mono">
      {status}
    </TintBadge>
  )
}

/**
 * A hostname you can open (planned_features #166): what an operator does
 * next with a name on screen is look at the site, so the badge is a link.
 * A pattern (`*.example.com`) is not an address and stays plain text. The
 * click stops at the link, so a row that opens something on click does not
 * open it too.
 */
export function HostLink({ host, children }: { host: string; children: React.ReactNode }) {
  if (!host || host.includes('*')) return <>{children}</>
  return (
    <a
      href={`https://${host}`}
      target="_blank"
      rel="noreferrer"
      title={`https://${host}`}
      onClick={(e) => e.stopPropagation()}
      onKeyDown={(e) => e.stopPropagation()}
      className="rounded-full outline-none hover:opacity-80 focus-visible:ring-2 focus-visible:ring-ring"
    >
      {children}
    </a>
  )
}
