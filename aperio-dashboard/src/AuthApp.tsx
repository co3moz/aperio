import { GlobeIcon, LockIcon, TriangleAlertIcon, UserIcon } from 'lucide-react'
import { useState, type FormEvent } from 'react'
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

// Only allow same-origin relative redirects to prevent open redirect abuse.
// Rejects protocol-relative URLs (//evil.com) and backslash-based bypasses.
function safeRedirect(url: string): string {
  if (url.startsWith('/') && !url.startsWith('//') && !url.startsWith('/\\')) {
    return url
  }
  return '/'
}

export function AuthApp() {
  const [username, setUsername] = useState('')
  const [password, setPassword] = useState('')
  const [error, setError] = useState(false)
  const [busy, setBusy] = useState(false)

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
      const res = await fetch(`/aperio/auth?redirect=${encodeURIComponent(dest)}`, {
        method: 'POST',
        headers: { Authorization: `Basic ${btoa(`${username}:${password}`)}` },
      })
      if (res.ok) {
        window.location.href = dest
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
            <GlobeIcon className="size-5" />
          </div>
          <CardTitle className="font-heading text-xl">Aperio</CardTitle>
          <CardDescription>Sign in to continue</CardDescription>
        </CardHeader>
        <CardContent>
          <form onSubmit={submit} className="grid gap-4">
            <div className="grid gap-2">
              <Label htmlFor="username">Username</Label>
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
              <Label htmlFor="password">Password</Label>
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
            <Button type="submit" size="lg" disabled={busy}>
              {busy && <Spinner />} Sign In
            </Button>
            {error && (
              <p className="flex items-center gap-2 rounded-2xl border border-red-500/30 bg-red-500/10 px-3 py-2 text-sm text-red-700 dark:text-red-400">
                <TriangleAlertIcon className="size-4 shrink-0" />
                Invalid credentials. Please try again.
              </p>
            )}
          </form>
        </CardContent>
      </Card>
    </div>
  )
}
