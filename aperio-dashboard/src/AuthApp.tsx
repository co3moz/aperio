import { FingerprintIcon, LockIcon, ShieldCheckIcon, TriangleAlertIcon, UserIcon } from 'lucide-react'
import { useEffect, useState, type FormEvent } from 'react'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { Spinner } from '@/components/ui/spinner'
import { AperioMark } from './components/AperioMark'
import { AperioWordmark } from './components/AperioWordmark'
import { LOGO_COLOR, logoDataUri } from '@/lib/logo'
import { useI18n } from '@/i18n'
import {
  browserSupportsPasskeys,
  passkeySignIn,
  passkeySignInDiscoverable,
  serverSupportsPasskeys,
} from '@/lib/webauthn'

// Only allow same-origin relative redirects to prevent open redirect abuse.
// Rejects protocol-relative URLs (//evil.com) and backslash-based bypasses.
function safeRedirect(url: string): string {
  if (url.startsWith('/') && !url.startsWith('//') && !url.startsWith('/\\')) {
    return url
  }
  return '/'
}

export function AuthApp() {
  const { t } = useI18n()
  const [username, setUsername] = useState('')
  const [password, setPassword] = useState('')
  const [totpCode, setTotpCode] = useState('')
  const [totpStep, setTotpStep] = useState(false)
  const [error, setError] = useState(false)
  const [busy, setBusy] = useState(false)
  const [passkeys, setPasskeys] = useState(false)
  const [passkeyError, setPasskeyError] = useState(false)

  useEffect(() => {
    if (!browserSupportsPasskeys()) return
    void serverSupportsPasskeys().then(setPasskeys)
  }, [])

  // The login page had no icon at all, so a tab waiting on a sign-in was an
  // anonymous blank. The dashboard tints the same mark by connection state;
  // here there is nothing to report yet, so it takes the brand colour.
  useEffect(() => {
    let link = document.querySelector<HTMLLinkElement>("link[rel='icon']")
    if (!link) {
      link = document.createElement('link')
      link.rel = 'icon'
      document.head.appendChild(link)
    }
    link.type = 'image/svg+xml'
    link.href = logoDataUri(LOGO_COLOR)
  }, [])

  const signInWithPasskey = async () => {
    setError(false)
    setPasskeyError(false)
    setBusy(true)
    const raw = new URLSearchParams(window.location.search).get('redirect') ?? '/'
    const dest = safeRedirect(raw)
    try {
      // With a username the classic flow runs; without one the authenticator's
      // account picker takes over (usernameless-enabled passkeys only).
      if (username.trim()) {
        await passkeySignIn(username.trim())
      } else {
        await passkeySignInDiscoverable()
      }
      window.location.href = dest
    } catch {
      setPasskeyError(true)
    } finally {
      setBusy(false)
    }
  }

  const submit = async (e: FormEvent) => {
    e.preventDefault()
    setError(false)
    setBusy(true)
    // Forward the intended destination so the server can pick the right
    // credentials (a client-set per-service password vs. the server's own) and
    // scope the session accordingly.
    const raw = new URLSearchParams(window.location.search).get('redirect') ?? '/'
    const dest = safeRedirect(raw)
    try {
      const headers: Record<string, string> = {
        Authorization: `Basic ${btoa(`${username}:${password}`)}`,
      }
      if (totpCode.trim()) headers['X-Aperio-Totp'] = totpCode.trim()
      const res = await fetch(`/aperio/auth?redirect=${encodeURIComponent(dest)}`, {
        method: 'POST',
        headers,
      })
      if (res.ok) {
        window.location.href = dest
        return
      }
      // The password was right but the account requires a TOTP code: switch
      // to the second-factor step instead of showing a credentials error.
      if (res.status === 401 && res.headers.get('x-aperio-totp') === 'required') {
        setTotpStep(true)
        setTotpCode('')
        return
      }
      setError(true)
    } catch {
      setError(true)
    } finally {
      setBusy(false)
    }
  }

  return (
    <div className="relative flex min-h-svh items-center justify-center overflow-hidden bg-background p-4">
      {/* Decorative watermark. `currentColor` at a low alpha means one element
          for both themes, and it is sized off the viewport's short side so it
          fills the page without ever widening it. `overflow-hidden` on the
          wrapper is what keeps the bleed from producing a scrollbar. */}
      <AperioMark
        aria-hidden
        className="pointer-events-none absolute size-[min(115vmin,1000px)] text-foreground/[0.045] select-none"
      />
      {/* Glass: the card drops its own fill and blurs what shows through, so
          the watermark behind it reads as frosted rather than as a picture
          the card is sitting on. The ring and shadow the card already carries
          are what still draw its edge, now that there is no fill to do it. */}
      {/* `bg-transparent` is not redundant next to `glass-surface`: Card ships
          `bg-card` in its own base, and tailwind-merge cannot know a custom
          utility is a background, so it keeps both and `bg-card` wins on
          source order. Naming the background here is what drops it. */}
      <Card className="glass-surface relative w-full max-w-sm bg-transparent">
        <CardHeader>
          {/* Mark and wordmark on one line: they are one lockup, and stacking
              them made the card open with two headings above the description. */}
          <div className="mb-2 flex items-center gap-3">
            {/* Same treatment as the sidebar: no tile, the mark standing on the
                card and taking the foreground colour, near-black on the light
                theme and near-white on the dark one. */}
            <div className="flex size-10 shrink-0 items-center justify-center">
              {totpStep ? (
                <ShieldCheckIcon className="size-7" />
              ) : (
                <AperioMark className="size-[34px]" />
              )}
            </div>
            <CardTitle>
              <AperioWordmark className="text-lg font-normal" />
            </CardTitle>
          </div>
          <CardDescription>
            {totpStep ? t('Enter the code from your authenticator app') : t('Sign in to continue')}
          </CardDescription>
        </CardHeader>
        <CardContent>
          {/* Enter in a field has to submit. Implicit submission should give
              that for free, and does not here, so the form asks for the submit
              itself. `requestSubmit` rather than calling the handler directly:
              it still runs the browser's own validation and fires one real
              submit event, so an empty field is reported the usual way instead
              of being posted. Scoped to inputs, so Enter on the passkey button
              still activates that button. */}
          <form
            onSubmit={submit}
            onKeyDown={(e) => {
              if (e.key !== 'Enter' || !(e.target instanceof HTMLInputElement)) return
              e.preventDefault()
              e.currentTarget.requestSubmit()
            }}
            className="grid gap-4"
          >
            {!totpStep && (
              <>
                <div className="grid gap-2">
                  <Label htmlFor="username">{t('Username')}</Label>
                  <div className="relative">
                    <UserIcon className="pointer-events-none absolute left-3 top-1/2 size-4 -translate-y-1/2 text-muted-foreground" />
                    <Input
                      id="username"
                      autoComplete="username"
                      required
                      autoFocus
                      value={username}
                      onChange={(e) => setUsername(e.target.value)}
                      className="pl-9"
                    />
                  </div>
                </div>
                <div className="grid gap-2">
                  <Label htmlFor="password">{t('Password')}</Label>
                  <div className="relative">
                    <LockIcon className="pointer-events-none absolute left-3 top-1/2 size-4 -translate-y-1/2 text-muted-foreground" />
                    <Input
                      id="password"
                      type="password"
                      autoComplete="current-password"
                      required
                      value={password}
                      onChange={(e) => setPassword(e.target.value)}
                      className="pl-9"
                    />
                  </div>
                </div>
              </>
            )}
            {totpStep && (
              <div className="grid gap-2">
                <Label htmlFor="totp">{t('Authentication code')}</Label>
                <div className="relative">
                  <ShieldCheckIcon className="pointer-events-none absolute left-3 top-1/2 size-4 -translate-y-1/2 text-muted-foreground" />
                  <Input
                    id="totp"
                    autoComplete="one-time-code"
                    inputMode="numeric"
                    required
                    autoFocus
                    placeholder="123456"
                    value={totpCode}
                    onChange={(e) => setTotpCode(e.target.value)}
                    className="pl-9"
                  />
                </div>
                <p className="text-xs text-muted-foreground">
                  {t('A recovery code also works here.')}
                </p>
              </div>
            )}
            <Button type="submit" size="lg" disabled={busy}>
              {busy && <Spinner />} {totpStep ? t('Verify') : t('Sign In')}
            </Button>
            {passkeys && !totpStep && (
              <Button
                type="button"
                variant="outline"
                size="lg"
                disabled={busy}
                onClick={() => void signInWithPasskey()}
              >
                <FingerprintIcon /> {t('Sign in with a passkey')}
              </Button>
            )}
            {passkeyError && (
              <p className="flex items-center gap-2 rounded-2xl border border-red-500/30 bg-red-500/10 px-3 py-2 text-sm text-red-700 dark:text-red-400">
                <TriangleAlertIcon className="size-4 shrink-0" />
                {t('Passkey sign-in failed. Enter your username above and try again.')}
              </p>
            )}
            {totpStep && (
              <Button
                type="button"
                variant="ghost"
                onClick={() => {
                  setTotpStep(false)
                  setTotpCode('')
                  setError(false)
                }}
              >
                {t('Back')}
              </Button>
            )}
            {error && (
              <p className="flex items-center gap-2 rounded-2xl border border-red-500/30 bg-red-500/10 px-3 py-2 text-sm text-red-700 dark:text-red-400">
                <TriangleAlertIcon className="size-4 shrink-0" />
                {totpStep
                  ? t('Invalid code. Please try again.')
                  : t('Invalid credentials. Please try again.')}
              </p>
            )}
          </form>
        </CardContent>
      </Card>
    </div>
  )
}
