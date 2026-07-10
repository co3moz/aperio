import { CheckIcon, CopyIcon } from 'lucide-react'
import { useState, type ReactNode } from 'react'
import { Button } from '@/components/ui/button'
import { Skeleton } from '@/components/ui/skeleton'
import { TableCell, TableRow } from '@/components/ui/table'
import { useI18n } from '@/i18n'
import { cn } from '@/lib/utils'

/** Centered empty-state row for tables. */
export function EmptyRow({
  colSpan,
  icon,
  children,
}: {
  colSpan: number
  icon?: ReactNode
  children: ReactNode
}) {
  return (
    <TableRow className="hover:bg-transparent">
      <TableCell colSpan={colSpan}>
        <div className="flex flex-col items-center justify-center gap-2 py-10 text-muted-foreground">
          {icon && <span className="[&_svg]:size-6 opacity-60">{icon}</span>}
          <span className="text-sm">{children}</span>
        </div>
      </TableCell>
    </TableRow>
  )
}

/** Placeholder shimmer rows shown while a table's first fetch is in flight. */
export function SkeletonRows({ rows, cols }: { rows: number; cols: number }) {
  return (
    <>
      {Array.from({ length: rows }).map((_, r) => (
        <TableRow key={r} className="hover:bg-transparent">
          {Array.from({ length: cols }).map((_, c) => (
            <TableCell key={c}>
              <Skeleton className="h-4 w-full max-w-24" />
            </TableCell>
          ))}
        </TableRow>
      ))}
    </>
  )
}

/** Copies `value` to the clipboard, flipping the icon briefly on success. */
export function CopyButton({
  value,
  label,
  className,
  size = 'xs',
}: {
  value: string
  label?: string
  className?: string
  size?: 'xs' | 'sm'
}) {
  const { t } = useI18n()
  const [copied, setCopied] = useState(false)
  const copy = async () => {
    try {
      await navigator.clipboard.writeText(value)
      setCopied(true)
      setTimeout(() => setCopied(false), 2000)
    } catch {
      // Clipboard may be unavailable; the value stays selectable in the UI.
    }
  }
  return (
    <Button variant="outline" size={size} className={className} onClick={copy}>
      {copied ? <CheckIcon /> : <CopyIcon />} {copied ? t('Copied') : (label ?? t('Copy'))}
    </Button>
  )
}

/** Section heading with an optional action area on the right. */
export function SectionHeader({
  title,
  description,
  children,
}: {
  title: string
  description?: string
  children?: ReactNode
}) {
  return (
    <div className="flex flex-wrap items-center justify-between gap-3">
      <div>
        <h2 className="font-heading text-lg font-semibold tracking-tight">{title}</h2>
        {description && <p className="text-sm text-muted-foreground">{description}</p>}
      </div>
      {children && <div className="flex flex-wrap items-center gap-2">{children}</div>}
    </div>
  )
}

/** Live/health indicator dot with the pulse animation. */
export function StatusDot({ active, className }: { active: boolean; className?: string }) {
  return (
    <span
      className={cn(
        'inline-block size-2 shrink-0 animate-pulse rounded-full motion-reduce:animate-none',
        active ? 'bg-emerald-500 shadow-[0_0_8px] shadow-emerald-500' : 'bg-red-500 shadow-[0_0_8px] shadow-red-500',
        className,
      )}
    />
  )
}

/** Preformatted block for headers/bodies/commands (inspector, wizard). */
export function PreBlock({ children, className }: { children: string; className?: string }) {
  return (
    <pre
      className={cn(
        'max-h-60 overflow-auto whitespace-pre-wrap break-all rounded-2xl border bg-muted/50 p-3 font-mono text-xs leading-relaxed',
        className,
      )}
    >
      {children}
    </pre>
  )
}
