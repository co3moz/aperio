import { MagnifyingGlassIcon } from '@radix-ui/react-icons'
import { Dialog, Flex, Text, TextField, VisuallyHidden } from '@radix-ui/themes'
import { useEffect, useState } from 'react'

export interface Command {
  id: string
  label: string
  hint?: string
  run: () => void
}

/**
 * A keyboard-driven command menu. Cmd/Ctrl+K toggles it; typing filters, the
 * arrow keys move the selection and Enter runs it. Navigation and appearance
 * commands are supplied by the caller.
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
  const [query, setQuery] = useState('')
  const [active, setActive] = useState(0)

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

  const needle = query.trim().toLowerCase()
  const filtered = needle
    ? commands.filter((c) => c.label.toLowerCase().includes(needle))
    : commands
  const clamped = Math.min(active, Math.max(filtered.length - 1, 0))

  const close = () => {
    onOpenChange(false)
    setQuery('')
    setActive(0)
  }

  const run = (command: Command | undefined) => {
    if (!command) return
    command.run()
    close()
  }

  const onKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === 'ArrowDown') {
      e.preventDefault()
      setActive((a) => Math.min(a + 1, filtered.length - 1))
    } else if (e.key === 'ArrowUp') {
      e.preventDefault()
      setActive((a) => Math.max(a - 1, 0))
    } else if (e.key === 'Enter') {
      e.preventDefault()
      run(filtered[clamped])
    }
  }

  return (
    <Dialog.Root
      open={open}
      onOpenChange={(next) => {
        if (!next) close()
        else onOpenChange(true)
      }}
    >
      <Dialog.Content maxWidth="480px" align="start">
        <VisuallyHidden>
          <Dialog.Title>Command menu</Dialog.Title>
        </VisuallyHidden>
        <TextField.Root
          autoFocus
          placeholder="Type a command…"
          value={query}
          onChange={(e) => {
            setQuery(e.target.value)
            setActive(0)
          }}
          onKeyDown={onKeyDown}
        >
          <TextField.Slot>
            <MagnifyingGlassIcon />
          </TextField.Slot>
        </TextField.Root>
        <Flex direction="column" gap="1" mt="3">
          {filtered.length === 0 ? (
            <Text size="2" color="gray" align="center" style={{ padding: 'var(--space-3)' }}>
              No matching commands
            </Text>
          ) : (
            filtered.map((c, i) => (
              <button
                key={c.id}
                type="button"
                className="command-item"
                data-active={i === clamped}
                onMouseEnter={() => setActive(i)}
                onClick={() => run(c)}
              >
                <Text size="2">{c.label}</Text>
                {c.hint && (
                  <Text size="1" color="gray">
                    {c.hint}
                  </Text>
                )}
              </button>
            ))
          )}
        </Flex>
      </Dialog.Content>
    </Dialog.Root>
  )
}
