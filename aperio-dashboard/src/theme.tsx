import '@radix-ui/themes/styles.css'
import './global.css'
import { Theme } from '@radix-ui/themes'
import { createContext, useCallback, useContext, useEffect, useState, type ReactNode } from 'react'
import { ToastProvider } from './hooks/useToast'

type Appearance = 'light' | 'dark'

const STORAGE_KEY = 'aperio-appearance'

/** Resolves the initial appearance: an explicit stored choice wins, otherwise
 *  the OS preference, defaulting to dark. */
function initialAppearance(): Appearance {
  try {
    const saved = localStorage.getItem(STORAGE_KEY)
    if (saved === 'light' || saved === 'dark') return saved
  } catch {
    // localStorage may be unavailable (private mode); fall through to the OS pref.
  }
  if (typeof window !== 'undefined' && window.matchMedia('(prefers-color-scheme: light)').matches) {
    return 'light'
  }
  return 'dark'
}

interface ThemeMode {
  appearance: Appearance
  toggle: () => void
}

const ThemeContext = createContext<ThemeMode>({ appearance: 'dark', toggle: () => {} })

export function useThemeMode(): ThemeMode {
  return useContext(ThemeContext)
}

export function AppTheme({ children }: { children: ReactNode }) {
  const [appearance, setAppearance] = useState<Appearance>(initialAppearance)

  useEffect(() => {
    try {
      localStorage.setItem(STORAGE_KEY, appearance)
    } catch {
      // Persisting the preference is best-effort.
    }
  }, [appearance])

  const toggle = useCallback(() => {
    setAppearance((a) => (a === 'dark' ? 'light' : 'dark'))
  }, [])

  return (
    <ThemeContext.Provider value={{ appearance, toggle }}>
      <Theme
        appearance={appearance}
        accentColor="indigo"
        grayColor="slate"
        radius="large"
        panelBackground="translucent"
      >
        <ToastProvider>{children}</ToastProvider>
      </Theme>
    </ThemeContext.Provider>
  )
}
