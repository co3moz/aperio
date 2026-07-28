import { CheckIcon, LanguagesIcon, MoonIcon, SunIcon } from 'lucide-react'
import { Button } from '@/components/ui/button'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import { Tooltip, TooltipContent, TooltipTrigger } from '@/components/ui/tooltip'
import { LANGUAGES, useI18n } from '@/i18n'
import { useThemeMode } from '@/theme'

/**
 * Language picker and theme toggle.
 *
 * Shared by the dashboard header and the login page, because both need to be
 * reachable *before* signing in: someone who cannot read the form cannot get
 * far enough to find the control that would fix it, and a login page that
 * ignores a dark-theme preference is the one page that flashes white.
 */
export function AppearanceControls({ align = 'end' }: { align?: 'start' | 'end' }) {
  const { t, lang, setLang } = useI18n()
  const { appearance, toggle } = useThemeMode()

  return (
    <>
      <DropdownMenu>
        <DropdownMenuTrigger
          render={<Button variant="ghost" size="icon-sm" aria-label={t('Change language')} />}
        >
          <LanguagesIcon />
        </DropdownMenuTrigger>
        <DropdownMenuContent align={align}>
          {LANGUAGES.map((l) => (
            <DropdownMenuItem key={l.code} onClick={() => setLang(l.code)}>
              <span className="flex-1">{l.label}</span>
              {lang === l.code && <CheckIcon className="size-4" />}
            </DropdownMenuItem>
          ))}
        </DropdownMenuContent>
      </DropdownMenu>
      <Tooltip>
        <TooltipTrigger
          render={
            <Button
              variant="ghost"
              size="icon-sm"
              onClick={toggle}
              aria-label={t('Toggle color theme')}
            />
          }
        >
          {appearance === 'dark' ? <SunIcon /> : <MoonIcon />}
        </TooltipTrigger>
        <TooltipContent>
          {appearance === 'dark' ? t('Switch to light theme') : t('Switch to dark theme')}
        </TooltipContent>
      </Tooltip>
    </>
  )
}
