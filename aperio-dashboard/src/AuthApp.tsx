import { FingerprintIcon, GlobeIcon, LockIcon, ShieldCheckIcon, TriangleAlertIcon, UserIcon } from 'lucide-react'
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
    <div className="flex min-h-svh items-center justify-center bg-background p-4">
      <Card className="w-full max-w-sm">
        <CardHeader>
          <div className="mb-2 flex size-10 items-center justify-center rounded-2xl bg-primary text-primary-foreground">
            {totpStep ? <ShieldCheckIcon className="size-5" /> : <GlobeIcon className="size-5" />}
          </div>
          <CardTitle className="font-heading text-xl">Aperio</CardTitle>
          <CardDescription>
            {totpStep ? t('Enter the code from your authenticator app') : t('Sign in to continue')}
          </CardDescription>
        </CardHeader>
        <CardContent>
          <form onSubmit={submit} className="grid gap-4">
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
