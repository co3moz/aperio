import type { LucideIcon } from 'lucide-react'
import { useEffect } from 'react'
import {
  CommandDialog,
  CommandEmpty,
  CommandGroup,
  CommandInput,
  CommandItem,
  CommandList,
  CommandShortcut,
} from '@/components/ui/command'

export interface Command {
  id: string
  label: string
  hint?: string
  icon?: LucideIcon
  run: () => void
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
      <CommandInput placeholder="Type a command…" />
      <CommandList>
        <CommandEmpty>No matching commands</CommandEmpty>
        <CommandGroup>
          {commands.map((c) => (
            <CommandItem
              key={c.id}
              value={c.label}
              onSelect={() => {
                c.run()
                onOpenChange(false)
              }}
            >
              {c.icon && <c.icon />}
              <span>{c.label}</span>
              {c.hint && <CommandShortcut>{c.hint}</CommandShortcut>}
            </CommandItem>
          ))}
        </CommandGroup>
      </CommandList>
    </CommandDialog>
  )
}
