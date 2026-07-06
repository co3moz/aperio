import { CheckIcon, CopyIcon, Link2Icon } from '@radix-ui/react-icons'
import {
  Button,
  Callout,
  Card,
  Code,
  Flex,
  Heading,
  Select,
  Text,
  TextField,
} from '@radix-ui/themes'
import { useState, type FormEvent } from 'react'
import { api, ApiError } from '../lib/api'

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
  const [copied, setCopied] = useState(false)

  const create = async (e: FormEvent) => {
    e.preventDefault()
    if (!hostname.trim()) return
    setBusy(true)
    setError(null)
    setResult(null)
    setCopied(false)
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

  const copy = async () => {
    if (!result) return
    try {
      await navigator.clipboard.writeText(result.url)
      setCopied(true)
      setTimeout(() => setCopied(false), 2000)
    } catch {
      // Clipboard unavailable; the URL below stays selectable.
    }
  }

  return (
    <Flex direction="column" gap="3">
      <Heading size="4">Share Links</Heading>
      <Card size="3">
        <Flex direction="column" gap="3">
          <Text size="2" color="gray">
            Give someone temporary access to an auth-protected site: the link carries a signed,
            expiring token scoped to the hostname (and optional path). Opening it sets a cookie
            and redirects to the clean URL. Links are stateless — they cannot be listed later,
            they simply expire.
          </Text>
          <form onSubmit={create}>
            <Flex gap="2" align="center" wrap="wrap">
              <div style={{ flex: 2, minWidth: 200 }}>
                <TextField.Root
                  value={hostname}
                  onChange={(e) => setHostname(e.target.value)}
                  placeholder="app.example.com"
                />
              </div>
              <div style={{ flex: 1, minWidth: 120 }}>
                <TextField.Root
                  value={path}
                  onChange={(e) => setPath(e.target.value)}
                  placeholder="/docs (optional)"
                />
              </div>
              <Select.Root value={ttl} onValueChange={setTtl}>
                <Select.Trigger />
                <Select.Content>
                  {TTL_OPTIONS.map((o) => (
                    <Select.Item key={o.value} value={o.value}>
                      {o.label}
                    </Select.Item>
                  ))}
                </Select.Content>
              </Select.Root>
              <Button type="submit" loading={busy} variant="soft">
                <Link2Icon /> Create link
              </Button>
            </Flex>
          </form>
          {error && (
            <Callout.Root color="red" size="1">
              <Callout.Text>{error}</Callout.Text>
            </Callout.Root>
          )}
          {result && (
            <Callout.Root color="green" size="1">
              <Callout.Text>
                <Flex align="center" gap="2" wrap="wrap">
                  <Code size="2" style={{ wordBreak: 'break-all' }}>
                    {result.url}
                  </Code>
                  <Button size="1" variant="soft" onClick={copy}>
                    {copied ? <CheckIcon /> : <CopyIcon />} {copied ? 'Copied' : 'Copy'}
                  </Button>
                  <Text size="1" color="gray">
                    {result.expires_at
                      ? `valid until ${new Date(result.expires_at * 1000).toLocaleString()}`
                      : 'never expires'}
                  </Text>
                </Flex>
              </Callout.Text>
            </Callout.Root>
          )}
        </Flex>
      </Card>
    </Flex>
  )
}
