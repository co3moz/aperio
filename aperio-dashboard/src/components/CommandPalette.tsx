import type { LucideIcon } from 'lucide-react'
import { useEffect } from 'react'
import {
  Command,
  CommandDialog,
  CommandEmpty,
  CommandGroup,
  CommandInput,
  CommandItem,
  CommandList,
  CommandShortcut,
} from '@/components/ui/command'
import { useI18n } from '@/i18n'

export interface Command {
  id: string
  label: string
  hint?: string
  icon?: LucideIcon
  /** Heading this command is listed under. Ungrouped commands come first. */
  group?: string
  /** Shown right-aligned: what this thing currently *is*, not what it does. */
  detail?: string
  /** Extra words that should match it, a setting's key, its group's name. */
  keywords?: string
  run: () => void
}

/** Commands in listing order, gathered under their headings. */
function groupsOf(commands: Command[]): [string, Command[]][] {
  const out = new Map<string, Command[]>()
  for (const c of commands) {
    const key = c.group ?? ''
    const list = out.get(key)
    if (list) list.push(c)
    else out.set(key, [c])
  }
  return [...out]
}

/**
 * Keyboard-driven command menu (cmdk). Cmd/Ctrl+K toggles it; typing filters,
 * arrows move the selection, Enter runs it.
 */
export function CommandPalette({
  open,
  onOpenChange,
  commands,
}: {
  open: boolean
  onOpenChange: (open: boolean) => void
  commands: Command[]
}) {
  const { t } = useI18n()
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === 'k') {
        e.preventDefault()
        onOpenChange(!open)
      }
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [open, onOpenChange])

  return (
    <CommandDialog open={open} onOpenChange={onOpenChange}>
      {/* The cmdk components need a <Command> root for their store; the
          Base UI CommandDialog wrapper does not provide one itself. */}
      <Command>
        <CommandInput placeholder={t('Type a command…')} />
        <CommandList>
          <CommandEmpty>{t('No matching commands')}</CommandEmpty>
          {groupsOf(commands).map(([heading, items]) => (
            <CommandGroup key={heading} heading={heading || undefined}>
              {items.map((c) => (
                <CommandItem
                  key={c.id}
                  // Everything worth matching on, not just the visible label:
                  // a setting is as likely to be searched for by its env key
                  // as by its English name.
                  value={[c.label, c.keywords, c.hint].filter(Boolean).join(' ')}
                  onSelect={() => {
                    c.run()
                    onOpenChange(false)
                  }}
                >
                  {c.icon && <c.icon />}
                  <span className="truncate">{c.label}</span>
                  {c.detail && (
                    <span className="ml-auto max-w-40 truncate font-mono text-xs text-muted-foreground">
                      {c.detail}
                    </span>
                  )}
                  {c.hint && <CommandShortcut>{c.hint}</CommandShortcut>}
                </CommandItem>
              ))}
            </CommandGroup>
          ))}
        </CommandList>
      </Command>
    </CommandDialog>
  )
}
