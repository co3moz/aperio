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
