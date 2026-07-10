import { createContext, useCallback, useContext, useEffect, useState, type ReactNode } from 'react'
import { de } from './de'
import { es } from './es'
import { fr } from './fr'
import { ja } from './ja'
import { ru } from './ru'
import { tr } from './tr'
import { zh } from './zh'

export const LANGUAGES = [
  { code: 'en', label: 'English' },
  { code: 'de', label: 'Deutsch' },
  { code: 'es', label: 'Español' },
  { code: 'fr', label: 'Français' },
  { code: 'tr', label: 'Türkçe' },
  { code: 'ru', label: 'Русский' },
  { code: 'zh', label: '中文' },
  { code: 'ja', label: '日本語' },
] as const

export type Lang = (typeof LANGUAGES)[number]['code']

// English is the source language: the translation KEY is the English string,
// so a missing entry falls back to English rather than a placeholder.
const DICTS: Record<Exclude<Lang, 'en'>, Record<string, string>> = {
  de,
  es,
  fr,
  tr,
  ru,
  zh,
  ja,
}

// A user's explicit choice lives for the browser session only; without one,
// detection runs again next session (browser language / server default).
const OVERRIDE_KEY = 'aperio-lang'

function isLang(value: string | null | undefined): value is Lang {
  return LANGUAGES.some((l) => l.code === value)
}

function storedOverride(): Lang | null {
  try {
    const saved = sessionStorage.getItem(OVERRIDE_KEY)
    return isLang(saved) ? saved : null
  } catch {
    return null
  }
}

/** First supported language from the browser's preference list. */
function browserLang(): Lang | null {
  const prefs = navigator.languages?.length ? navigator.languages : [navigator.language]
  for (const raw of prefs) {
    const code = raw?.slice(0, 2).toLowerCase()
    if (isLang(code)) return code
  }
  return null
}

/** Interpolates `{name}` placeholders. */
function interpolate(template: string, vars?: Record<string, string | number>): string {
  if (!vars) return template
  return template.replace(/\{(\w+)\}/g, (m, name: string) =>
    name in vars ? String(vars[name]) : m,
  )
}

export type TFn = (key: string, vars?: Record<string, string | number>) => string

interface I18nContextValue {
  lang: Lang
  setLang: (lang: Lang) => void
  t: TFn
}

const I18nContext = createContext<I18nContextValue>({
  lang: 'en',
  setLang: () => {},
  t: (key, vars) => interpolate(key, vars),
})

export function useI18n(): I18nContextValue {
  return useContext(I18nContext)
}

export function I18nProvider({ children }: { children: ReactNode }) {
  // Session override wins, then the browser language; when neither decides,
  // the server's configured default (fetched below) does, else English.
  const [lang, setLangState] = useState<Lang>(() => storedOverride() ?? browserLang() ?? 'en')
  const [decided, setDecided] = useState(() => storedOverride() !== null || browserLang() !== null)

  useEffect(() => {
    if (decided) return
    let cancelled = false
    fetch('/aperio/health')
      .then((res) => (res.ok ? res.json() : null))
      .then((info: { ui_language?: string } | null) => {
        if (!cancelled && info && isLang(info.ui_language)) setLangState(info.ui_language)
      })
      .catch(() => {
        // Unreachable server: English stays.
      })
    return () => {
      cancelled = true
    }
  }, [decided])

  const setLang = useCallback((next: Lang) => {
    setLangState(next)
    setDecided(true)
    try {
      sessionStorage.setItem(OVERRIDE_KEY, next)
    } catch {
      // Session storage may be unavailable; the choice just won't stick.
    }
  }, [])

  useEffect(() => {
    document.documentElement.lang = lang
  }, [lang])

  const t = useCallback<TFn>(
    (key, vars) => {
      const template = lang === 'en' ? key : (DICTS[lang][key] ?? key)
      return interpolate(template, vars)
    },
    [lang],
  )

  return <I18nContext.Provider value={{ lang, setLang, t }}>{children}</I18nContext.Provider>
}
