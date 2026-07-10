import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useState,
  type ReactNode,
} from 'react'
import { Toaster } from '@/components/ui/sonner'
import { TooltipProvider } from '@/components/ui/tooltip'
import { I18nProvider } from '@/i18n'

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

/** Current appearance + toggle, for the header button and command menu. */
export function useThemeMode(): ThemeMode {
  return useContext(ThemeContext)
}

/**
 * App-wide providers: dark/light theme via a `dark` class on <html>
 * (next-themes injects an inline script the dashboard CSP would block, so
 * this is hand-rolled), tooltips, and the toast viewport.
 */
export function AppProviders({ children }: { children: ReactNode }) {
  const [appearance, setAppearance] = useState<Appearance>(initialAppearance)

  useEffect(() => {
    document.documentElement.classList.toggle('dark', appearance === 'dark')
    document.documentElement.style.colorScheme = appearance
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
      <I18nProvider>
        <TooltipProvider delay={300}>
          {children}
          <Toaster position="bottom-right" theme={appearance} />
        </TooltipProvider>
      </I18nProvider>
    </ThemeContext.Provider>
  )
}
