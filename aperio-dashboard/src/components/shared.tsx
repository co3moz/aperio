import { CheckIcon, CopyIcon } from 'lucide-react'
import { useMemo, useState, type KeyboardEvent, type ReactNode } from 'react'
import { Button } from '@/components/ui/button'
import { Skeleton } from '@/components/ui/skeleton'
import { TableCell, TableRow } from '@/components/ui/table'
import { useI18n } from '@/i18n'
import { highlight } from '@/lib/highlight'
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
        <div className="flex flex-col items-center justify-center gap-2 px-6 py-10 text-muted-foreground">
          {icon && <span className="[&_svg]:size-6 opacity-60">{icon}</span>}
          {/* Centred and held to a readable width. A sentence long enough to
              wrap would otherwise run the full width of the container and sit
              left of an icon that is centred, which reads as a layout bug. */}
          <span className="max-w-md text-center text-sm text-balance">{children}</span>
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

/**
 * A list of records as stacked rows rather than a table.
 *
 * A table spends the width it has on columns, and inside the settings dialog
 * there is not enough of it: six columns in a pane that narrow leave every
 * cell too cramped to read, and the widest value (a hostname list, a user
 * agent) decides how squeezed the rest are. Stacking gives each record the
 * full width one line at a time, and drops the header row that a handful of
 * records did not need anyway.
 *
 * Deliberately unfilled: the dialog behind it is a glass panel, and an opaque
 * card would put a solid sheet over the whole thing.
 */
export function RecordList({ children, className }: { children: ReactNode; className?: string }) {
  return (
    <div className={cn('divide-y overflow-hidden rounded-3xl border', className)}>{children}</div>
  )
}

/**
 * One record: what it is on the first line, what is true about it on the
 * second, and what can be done to it on the right.
 */
export function RecordRow({
  title,
  actions,
  children,
}: {
  title: ReactNode
  actions?: ReactNode
  children?: ReactNode
}) {
  return (
    <div className="flex flex-wrap items-start justify-between gap-x-3 gap-y-2 px-4 py-3">
      {/* `basis-56` rather than auto width: a long value on the detail line (a
          webhook URL, a hostname list) would otherwise widen this column until
          the actions wrapped underneath it. Truncating the value is the better
          trade, it has a `title`, and the buttons stay where they were on the
          row above. */}
      <div className="flex min-w-0 flex-1 basis-56 flex-col gap-1">
        <div className="flex flex-wrap items-center gap-1.5 text-sm font-medium">{title}</div>
        {children && (
          <div className="flex flex-wrap items-center gap-x-3 gap-y-1 text-xs text-muted-foreground">
            {children}
          </div>
        )}
      </div>
      {actions && <div className="flex shrink-0 flex-wrap items-center gap-1">{actions}</div>}
    </div>
  )
}

/** One fact on a record's detail line, with an optional leading icon. */
export function RecordFact({
  icon,
  title,
  className,
  children,
}: {
  icon?: ReactNode
  title?: string
  className?: string
  children: ReactNode
}) {
  return (
    <span className={cn('inline-flex min-w-0 items-center gap-1', className)} title={title}>
      {icon && <span className="shrink-0 [&_svg]:size-3.5">{icon}</span>}
      <span className="truncate">{children}</span>
    </span>
  )
}

/** Centered empty state for a `RecordList`. */
export function RecordEmpty({ icon, children }: { icon?: ReactNode; children: ReactNode }) {
  return (
    <div className="flex flex-col items-center justify-center gap-2 px-6 py-10 text-muted-foreground">
      {icon && <span className="opacity-60 [&_svg]:size-6">{icon}</span>}
      <span className="max-w-md text-center text-sm text-balance">{children}</span>
    </div>
  )
}

/** Placeholder rows shown while a `RecordList`'s first fetch is in flight. */
export function RecordSkeleton({ rows }: { rows: number }) {
  return (
    <>
      {Array.from({ length: rows }).map((_, r) => (
        <div key={r} className="flex flex-col gap-2 px-4 py-3.5">
          <Skeleton className="h-4 w-40" />
          <Skeleton className="h-3 w-64 max-w-full" />
        </div>
      ))}
    </>
  )
}

/**
 * Enter in a text field runs the dialog's confirm action.
 *
 * These dialogs are built from labelled inputs and a footer button rather
 * than a real `<form>`, so nothing gives them the one behaviour every filled-in
 * form has: typing the last value and pressing Enter. Put it on the element
 * wrapping the fields. Textareas and buttons keep Enter for themselves, a
 * newline and a click are what it means there.
 */
export function submitOnEnter(run: () => void) {
  return (e: KeyboardEvent) => {
    if (e.key !== 'Enter') return
    if ((e.target as HTMLElement).tagName !== 'INPUT') return
    e.preventDefault()
    run()
  }
}

/** Preformatted block for headers/bodies/commands (inspector, wizard). */
/** Colour per token kind. Chosen from the theme's own variables rather than
 *  fixed hex values so highlighting follows light and dark mode, and reads as
 *  part of the dashboard instead of as a code editor pasted into it. */
const TOKEN_CLASS: Record<string, string> = {
  key: 'text-[var(--primary)]',
  string: 'text-[oklch(0.62_0.13_150)] dark:text-[oklch(0.75_0.13_150)]',
  number: 'text-[oklch(0.6_0.15_25)] dark:text-[oklch(0.75_0.13_25)]',
  literal: 'text-[oklch(0.6_0.15_300)] dark:text-[oklch(0.76_0.13_300)]',
  punct: 'text-muted-foreground',
  tag: 'text-[var(--primary)]',
  attr: 'text-[oklch(0.6_0.15_60)] dark:text-[oklch(0.78_0.12_60)]',
  comment: 'text-muted-foreground italic',
}

/** A `PreBlock` whose content is tokenized when it is JSON, XML or HTML.
 *
 * Falls back to exactly what `PreBlock` renders otherwise, including for a
 * body too large to tokenize, so nothing that was readable before becomes
 * less so. */
export function HighlightedBlock({
  children,
  className,
}: {
  children: string
  className?: string
}) {
  const { language, tokens } = useMemo(() => highlight(children), [children])
  if (language === 'text') return <PreBlock className={className}>{children}</PreBlock>
  return (
    <pre
      className={cn(
        'max-h-60 overflow-auto whitespace-pre-wrap break-all rounded-2xl border bg-muted/50 p-3 font-mono text-xs leading-relaxed',
        className,
      )}
    >
      {tokens.map((token, i) => (
        <span key={i} className={token.kind ? TOKEN_CLASS[token.kind] : undefined}>
          {token.text}
        </span>
      ))}
    </pre>
  )
}

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
