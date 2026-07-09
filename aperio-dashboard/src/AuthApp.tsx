import { ExclamationTriangleIcon, LockClosedIcon, PersonIcon } from '@radix-ui/react-icons'
import { Box, Button, Callout, Card, Flex, Heading, Text, TextField } from '@radix-ui/themes'
import { useState, type FormEvent } from 'react'

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
    <Flex align="center" justify="center" minHeight="100vh" p="4">
      <Card size="4" style={{ width: '100%', maxWidth: 400 }}>
        <Heading size="6">Aperio</Heading>
        <Text as="p" size="2" color="gray" mt="1" mb="5">
          Sign in to continue
        </Text>
        <form onSubmit={submit}>
          <Flex direction="column" gap="4">
            <Box>
              <Text as="label" size="1" weight="medium" color="gray" htmlFor="username">
                USERNAME
              </Text>
              <TextField.Root
                id="username"
                mt="1"
                size="3"
                autoComplete="username"
                required
                autoFocus
                value={username}
                onChange={(e) => setUsername(e.target.value)}
              >
                <TextField.Slot>
                  <PersonIcon />
                </TextField.Slot>
              </TextField.Root>
            </Box>
            <Box>
              <Text as="label" size="1" weight="medium" color="gray" htmlFor="password">
                PASSWORD
              </Text>
              <TextField.Root
                id="password"
                mt="1"
                size="3"
                type="password"
                autoComplete="current-password"
                required
                value={password}
                onChange={(e) => setPassword(e.target.value)}
              >
                <TextField.Slot>
                  <LockClosedIcon />
                </TextField.Slot>
              </TextField.Root>
            </Box>
            <Button size="3" type="submit" loading={busy}>
              Sign In
            </Button>
            {error && (
              <Callout.Root color="red" size="1">
                <Callout.Icon>
                  <ExclamationTriangleIcon />
                </Callout.Icon>
                <Callout.Text>Invalid credentials. Please try again.</Callout.Text>
              </Callout.Root>
            )}
          </Flex>
        </form>
      </Card>
    </Flex>
  )
}
