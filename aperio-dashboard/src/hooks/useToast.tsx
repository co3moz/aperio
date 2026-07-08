import { CheckCircledIcon, CrossCircledIcon, Cross2Icon, InfoCircledIcon } from '@radix-ui/react-icons'
import { Flex, IconButton, Text } from '@radix-ui/themes'
import { createContext, useCallback, useContext, useRef, useState, type ReactNode } from 'react'

type ToastColor = 'green' | 'red' | 'gray'

interface ToastItem {
  id: number
  message: string
  color: ToastColor
}

type ToastFn = (message: string, color?: ToastColor) => void

const ToastContext = createContext<ToastFn>(() => {})

/** Shows a transient notification in the bottom-right corner. */
export function useToast(): ToastFn {
  return useContext(ToastContext)
}

const TIMEOUT_MS = 4000

const ICONS: Record<ToastColor, ReactNode> = {
  green: <CheckCircledIcon />,
  red: <CrossCircledIcon />,
  gray: <InfoCircledIcon />,
}

export function ToastProvider({ children }: { children: ReactNode }) {
  const [items, setItems] = useState<ToastItem[]>([])
  const nextId = useRef(1)

  const dismiss = useCallback((id: number) => {
    setItems((list) => list.filter((t) => t.id !== id))
  }, [])

  const toast = useCallback<ToastFn>(
    (message, color = 'gray') => {
      const id = nextId.current++
      setItems((list) => [...list, { id, message, color }])
      setTimeout(() => dismiss(id), TIMEOUT_MS)
    },
    [dismiss],
  )

  return (
    <ToastContext.Provider value={toast}>
      {children}
      {items.length > 0 && (
        <Flex
          direction="column"
          gap="2"
          className="toast-viewport"
          role="status"
          aria-live="polite"
        >
          {items.map((t) => (
            <Flex key={t.id} align="center" gap="2" className={`toast-item toast-${t.color}`}>
              <Text className="toast-icon" style={{ display: 'inline-flex' }}>
                {ICONS[t.color]}
              </Text>
              <Text size="2" style={{ flex: 1 }}>
                {t.message}
              </Text>
              <IconButton
                size="1"
                variant="ghost"
                color="gray"
                onClick={() => dismiss(t.id)}
                aria-label="Dismiss notification"
              >
                <Cross2Icon />
              </IconButton>
            </Flex>
          ))}
        </Flex>
      )}
    </ToastContext.Provider>
  )
}
