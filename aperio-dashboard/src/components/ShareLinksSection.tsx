import { Link2Icon } from 'lucide-react'
import { useState, type FormEvent } from 'react'
import { CopyButton, SectionHeader } from './shared'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import { Input } from '@/components/ui/input'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { Spinner } from '@/components/ui/spinner'
import { api, ApiError } from '@/lib/api'

const MINUTE = 60
const HOUR = 60 * MINUTE
const DAY = 24 * HOUR

const TTL_OPTIONS = [
  { value: String(10 * MINUTE), label: '10 minutes' },
  { value: String(30 * MINUTE), label: '30 minutes' },
  { value: String(HOUR), label: '1 hour' },
  { value: String(3 * HOUR), label: '3 hours' },
  { value: String(6 * HOUR), label: '6 hours' },
  { value: String(12 * HOUR), label: '12 hours' },
  { value: String(DAY), label: '1 day' },
  { value: String(2 * DAY), label: '2 days' },
  { value: String(3 * DAY), label: '3 days' },
  { value: String(5 * DAY), label: '5 days' },
  { value: String(7 * DAY), label: '1 week' },
  { value: String(14 * DAY), label: '2 weeks' },
  { value: String(30 * DAY), label: '1 month' },
  { value: String(90 * DAY), label: '3 months' },
  { value: String(180 * DAY), label: '6 months' },
  { value: String(365 * DAY), label: '1 year' },
  { value: String(2 * 365 * DAY), label: '2 years' },
  { value: String(5 * 365 * DAY), label: '5 years' },
  { value: '0', label: 'never expires' },
]

/**
 * Generates signed share links: a URL granting temporary access to an
 * auth-protected proxied site, scoped to a hostname (and optional path
 * prefix). Stateless on the server — links simply expire.
 */
export function ShareLinksSection() {
  const [hostname, setHostname] = useState('')
  const [path, setPath] = useState('')
  const [ttl, setTtl] = useState(TTL_OPTIONS[2].value)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [result, setResult] = useState<{ url: string; expires_at: number | null } | null>(null)

  const create = async (e: FormEvent) => {
    e.preventDefault()
    if (!hostname.trim()) return
    setBusy(true)
    setError(null)
    setResult(null)
    try {
      const created = await api.createShareLink({
        hostname: hostname.trim(),
        ...(path.trim() && path.trim() !== '/' ? { path: path.trim() } : {}),
        ttl_seconds: Number(ttl),
      })
      setResult(created)
    } catch (err) {
      setError(err instanceof ApiError ? err.message : String(err))
    } finally {
      setBusy(false)
    }
  }

  return (
    <section className="flex flex-col gap-3">
      <SectionHeader title="Share Links" />
      <Card className="py-5">
        <CardContent className="flex flex-col gap-4 px-5">
          <p className="text-sm text-muted-foreground">
            Give someone temporary access to an auth-protected site: the link carries a signed,
            expiring token scoped to the hostname (and optional path). Opening it sets a cookie
            and redirects to the clean URL. Links are stateless — they cannot be listed later,
            they simply expire.
          </p>
          <form onSubmit={create} className="flex flex-wrap items-center gap-2">
            <Input
              value={hostname}
              onChange={(e) => setHostname(e.target.value)}
              placeholder="app.example.com"
              className="min-w-52 flex-2"
            />
            <Input
              value={path}
              onChange={(e) => setPath(e.target.value)}
              placeholder="/docs (optional)"
              className="min-w-32 flex-1"
            />
            <Select value={ttl} onValueChange={(v) => setTtl(v as string)}>
              <SelectTrigger className="w-36">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {TTL_OPTIONS.map((o) => (
                  <SelectItem key={o.value} value={o.value}>
                    {o.label}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
            <Button type="submit" disabled={busy}>
              {busy ? <Spinner /> : <Link2Icon />} Create link
            </Button>
          </form>
          {error && <p className="text-sm text-destructive">{error}</p>}
          {result && (
            <div className="flex flex-wrap items-center gap-2 rounded-3xl border border-emerald-500/30 bg-emerald-500/10 px-4 py-3">
              <code className="min-w-0 break-all font-mono text-sm">{result.url}</code>
              <CopyButton value={result.url} />
              <span className="text-xs text-muted-foreground">
                {result.expires_at
                  ? `valid until ${new Date(result.expires_at * 1000).toLocaleString()}`
                  : 'never expires'}
              </span>
            </div>
          )}
        </CardContent>
      </Card>
    </section>
  )
}
