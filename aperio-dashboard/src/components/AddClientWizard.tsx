import { CheckIcon, CopyIcon, PlusIcon } from '@radix-ui/react-icons'
import {
  Button,
  Callout,
  Dialog,
  Flex,
  SegmentedControl,
  Tabs,
  Text,
  TextField,
} from '@radix-ui/themes'
import { useState } from 'react'
import { api, ApiError } from '../lib/api'

const CLIENT_IMAGE = 'ghcr.io/co3moz/aperio-client:latest'
const TOKEN_PLACEHOLDER = '<YOUR_APERIO_TOKEN>'

interface WizardState {
  target: string
  hostname: string
  path: string
  tokenName: string
}

/** Extracts the port when the target is a plain http://localhost:<port>. */
function localPort(target: string): string | null {
  const m = target.trim().match(/^http:\/\/(?:localhost|127\.0\.0\.1):(\d+)\/?$/)
  return m ? m[1] : null
}

function dockerCommand(serverUrl: string, token: string, s: WizardState): string {
  const lines = [
    'docker run -d --name aperio-client --restart unless-stopped --network host \\',
    `  -e APERIO_SERVER_URL=${serverUrl} \\`,
    `  -e APERIO_SERVER_TOKEN=${token} \\`,
    `  -e APERIO_CLIENT_TARGET=${s.target} \\`,
  ]
  if (s.hostname) lines.push(`  -e APERIO_HOSTNAME_BIND=${s.hostname} \\`)
  if (s.path) lines.push(`  -e APERIO_PATH_BIND=${s.path} \\`)
  lines.push(`  ${CLIENT_IMAGE}`)
  return lines.join('\n')
}

function cliCommand(serverUrl: string, token: string, s: WizardState): string {
  const port = localPort(s.target)
  // A local port or any URL works as the positional target.
  const target = port ?? s.target
  // Multi-line with continuations: long single lines wrap unpredictably
  // inside the <pre> block.
  const lines = [
    `aperio-client ${target} \\`,
    `  --server-url ${serverUrl} \\`,
    `  --server-token ${token}`,
  ]
  if (s.hostname) {
    lines[lines.length - 1] += ' \\'
    lines.push(`  --hostname ${s.hostname}`)
  }
  if (s.path) {
    lines[lines.length - 1] += ' \\'
    lines.push(`  --path ${s.path}`)
  }
  return lines.join('\n')
}

function yamlConfig(serverUrl: string, token: string, s: WizardState): string {
  const lines = [
    '# aperio.yaml',
    'server:',
    `  url: ${serverUrl}`,
    `  token: ${token}`,
    `target: ${s.target}`,
  ]
  if (s.hostname) lines.push(`hostname: ${s.hostname}`)
  if (s.path) lines.push(`path: ${s.path}`)
  lines.push('', '# then run:', '#   aperio-client')
  return lines.join('\n')
}

function CommandBlock({ content }: { content: string }) {
  const [copied, setCopied] = useState(false)

  const copy = async () => {
    try {
      await navigator.clipboard.writeText(content)
      setCopied(true)
      setTimeout(() => setCopied(false), 2000)
    } catch {
      // Clipboard unavailable; the text below stays selectable.
    }
  }

  // The copy button floats over the top-right corner of the code block so it
  // never stacks with the dialog's own footer buttons.
  return (
    <div style={{ position: 'relative', marginTop: 'var(--space-3)' }}>
      <pre className="inspector-pre" style={{ maxHeight: 320, paddingRight: 96 }}>
        {content}
      </pre>
      <Button
        size="1"
        variant="soft"
        onClick={copy}
        style={{ position: 'absolute', top: 8, right: 8 }}
      >
        {copied ? <CheckIcon /> : <CopyIcon />} {copied ? 'Copied' : 'Copy'}
      </Button>
    </div>
  )
}

/**
 * Copy-paste onboarding wizard: pick a token strategy, describe the local
 * service, and get ready-to-run docker / CLI / aperio.yaml snippets.
 */
export function AddClientWizard() {
  const [open, setOpen] = useState(false)
  const [state, setState] = useState<WizardState>({
    target: 'http://localhost:3000',
    hostname: '',
    path: '',
    tokenName: '',
  })
  const [tokenMode, setTokenMode] = useState<'existing' | 'mint'>('existing')
  const [mintedSecret, setMintedSecret] = useState<string | null>(null)
  const [minting, setMinting] = useState(false)
  const [error, setError] = useState<string | null>(null)

  const serverUrl = window.location.origin

  // A minted token is scoped to the binds entered at mint time; invalidate
  // the embedded secret when the scope changes afterwards.
  const set = (key: keyof WizardState) => (e: React.ChangeEvent<HTMLInputElement>) => {
    setState((s) => ({ ...s, [key]: e.target.value }))
    if (key === 'hostname' || key === 'path') setMintedSecret(null)
  }

  const openDialog = (next: boolean) => {
    if (next) {
      setMintedSecret(null)
      setError(null)
    }
    setOpen(next)
  }

  const mint = async () => {
    setMinting(true)
    setError(null)
    try {
      const created = await api.createToken({
        name: state.tokenName.trim() || 'client',
        hostnames: state.hostname ? [state.hostname.trim()] : ['*'],
        paths: state.path ? [state.path.trim()] : ['*'],
        allowed_ips: [],
      })
      setMintedSecret(created.token)
    } catch (e) {
      setError(e instanceof ApiError ? e.message : String(e))
    } finally {
      setMinting(false)
    }
  }

  const token = tokenMode === 'mint' && mintedSecret ? mintedSecret : TOKEN_PLACEHOLDER

  const field = (
    label: string,
    key: keyof WizardState,
    placeholder: string,
  ) => (
    <label>
      <Text as="div" size="1" weight="medium" color="gray" mb="1">
        {label}
      </Text>
      <TextField.Root value={state[key]} onChange={set(key)} placeholder={placeholder} />
    </label>
  )

  return (
    <Dialog.Root open={open} onOpenChange={openDialog}>
      <Dialog.Trigger>
        <Button size="2" variant="soft">
          <PlusIcon /> Connect a new client
        </Button>
      </Dialog.Trigger>
      <Dialog.Content maxWidth="620px">
        <Dialog.Title>Add a tunnel client</Dialog.Title>
        <Dialog.Description size="2" color="gray">
          Describe the local service; copy a ready-to-run command below.
        </Dialog.Description>

        <Flex direction="column" gap="3" mt="4">
          {field('LOCAL TARGET (WHERE YOUR SERVICE LISTENS)', 'target', 'http://localhost:3000')}
          <Flex gap="3">
            <div style={{ flex: 1 }}>{field('HOSTNAME BIND (OPTIONAL)', 'hostname', 'app.example.com')}</div>
            <div style={{ flex: 1 }}>{field('PATH BIND (OPTIONAL)', 'path', '/api')}</div>
          </Flex>

          <Flex direction="column" gap="2">
            <Text size="1" weight="medium" color="gray">
              TOKEN
            </Text>
            <SegmentedControl.Root
              value={tokenMode}
              onValueChange={(v) => setTokenMode(v as 'existing' | 'mint')}
            >
              <SegmentedControl.Item value="existing">I have a token</SegmentedControl.Item>
              <SegmentedControl.Item value="mint">Mint a scoped token now</SegmentedControl.Item>
            </SegmentedControl.Root>
            {tokenMode === 'existing' ? (
              <Text size="1" color="gray">
                The commands below use a <code>{TOKEN_PLACEHOLDER}</code> placeholder — replace it
                with your master token or an existing dynamic token.
              </Text>
            ) : mintedSecret ? (
              <Callout.Root size="1" color="green">
                <Callout.Text>
                  Token minted and embedded below. It is scoped to{' '}
                  {state.hostname || state.path
                    ? `${state.hostname || 'any hostname'} / ${state.path || 'any path'}`
                    : 'all binds'}{' '}
                  and will not be shown again after this dialog closes.
                </Callout.Text>
              </Callout.Root>
            ) : (
              <Flex gap="2" align="center">
                <div style={{ flex: 1 }}>
                  <TextField.Root
                    value={state.tokenName}
                    onChange={set('tokenName')}
                    placeholder="token name (e.g. staging-box)"
                  />
                </div>
                <Button onClick={mint} loading={minting}>
                  Mint token
                </Button>
              </Flex>
            )}
            {error && (
              <Callout.Root color="red" size="1">
                <Callout.Text>{error}</Callout.Text>
              </Callout.Root>
            )}
          </Flex>

          <Tabs.Root defaultValue="docker">
            <Tabs.List>
              <Tabs.Trigger value="docker">Docker</Tabs.Trigger>
              <Tabs.Trigger value="cli">CLI</Tabs.Trigger>
              <Tabs.Trigger value="yaml">aperio.yaml</Tabs.Trigger>
            </Tabs.List>
            <Tabs.Content value="docker">
              <CommandBlock content={dockerCommand(serverUrl, token, state)} />
            </Tabs.Content>
            <Tabs.Content value="cli">
              <CommandBlock content={cliCommand(serverUrl, token, state)} />
            </Tabs.Content>
            <Tabs.Content value="yaml">
              <CommandBlock content={yamlConfig(serverUrl, token, state)} />
            </Tabs.Content>
          </Tabs.Root>
        </Flex>

        <Flex mt="4" justify="end">
          <Dialog.Close>
            <Button variant="soft" color="gray">
              Close
            </Button>
          </Dialog.Close>
        </Flex>
      </Dialog.Content>
    </Dialog.Root>
  )
}
